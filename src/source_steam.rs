use std::{
    env,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

use vformats::{Limits, keyvalues};

const MAX_CONFIG_BYTES: u64 = 2 * 1024 * 1024;
const SOURCE_GAMES: &[(&str, &[&str])] = &[
    ("240", &["cstrike", "hl2"]),
    ("232330", &["cstrike", "hl2"]),
    ("730", &["csgo", "platform"]),
    ("220", &["hl2", "episodic", "ep2"]),
    ("380", &["episodic", "hl2"]),
    ("420", &["ep2", "hl2"]),
    ("320", &["hl2mp", "hl2"]),
    ("440", &["tf", "hl2"]),
    ("243750", &["hl2", "episodic", "ep2"]),
];

pub(crate) fn material_roots() -> &'static [PathBuf] {
    static ROOTS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut installations = Vec::new();
        if let Some(path) = env::var_os("STEAM_DIR") {
            installations.push(PathBuf::from(path));
        }
        #[cfg(windows)]
        if let Some(path) = windows_steam_directory() {
            installations.push(path);
        }
        #[cfg(windows)]
        for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Some(path) = env::var_os(variable) {
                installations.push(PathBuf::from(path).join("Steam"));
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
            for folder in [
                ".steam/steam",
                ".local/share/Steam",
                ".var/app/com.valvesoftware.Steam/.local/share/Steam",
            ] {
                installations.push(home.join(folder));
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
            installations.push(home.join("Library/Application Support/Steam"));
        }
        find_games(&installations)
    })
}

#[cfg(windows)]
fn windows_steam_directory() -> Option<PathBuf> {
    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::OsStringExt;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            key: isize,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut c_void,
            size: *mut u32,
        ) -> i32;
    }

    let subkey: Vec<_> = "Software\\Valve\\Steam".encode_utf16().chain([0]).collect();
    let value: Vec<_> = "SteamPath".encode_utf16().chain([0]).collect();
    let mut path = [0u16; 4096];
    let mut size = size_of_val(&path) as u32;
    // HKEY_CURRENT_USER and RRF_RT_REG_SZ. Windows writes at most the supplied buffer size.
    let status = unsafe {
        RegGetValueW(
            0x8000_0001u32 as i32 as isize,
            subkey.as_ptr(),
            value.as_ptr(),
            0x0000_0002,
            std::ptr::null_mut(),
            path.as_mut_ptr().cast(),
            &mut size,
        )
    };
    let len = path.iter().position(|unit| *unit == 0)?;
    (status == 0 && len > 0).then(|| PathBuf::from(OsString::from_wide(&path[..len])))
}

fn find_games(installations: &[PathBuf]) -> Vec<PathBuf> {
    let mut libraries = Vec::new();
    for steam in installations {
        add_directory(&mut libraries, steam.join("steamapps"));
        let Some(text) = read_config(&steam.join("steamapps/libraryfolders.vdf")) else {
            continue;
        };
        let Some(vdf) = parse(&text) else { continue };
        let Some(folders) = vdf.blocks("libraryfolders").next() else {
            continue;
        };
        for pair in &folders.pairs {
            if !pair.key.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let path = pair.value.as_str().or_else(|| {
                pair.value
                    .as_block()
                    .and_then(|library| library.get_str("path"))
            });
            if let Some(path) = path.map(Path::new).filter(|path| path.is_absolute()) {
                add_directory(&mut libraries, path.join("steamapps"));
            }
        }
    }

    let mut roots = Vec::new();
    for &(appid, game_folders) in SOURCE_GAMES {
        for library in &libraries {
            let Some(text) = read_config(&library.join(format!("appmanifest_{appid}.acf"))) else {
                continue;
            };
            let Some(vdf) = parse(&text) else { continue };
            let Some(app) = vdf.blocks("AppState").next() else {
                continue;
            };
            let Some(folder) = app.get_str("installdir") else {
                continue;
            };
            let mut components = Path::new(folder).components();
            if app.get_str("appid") != Some(appid)
                || !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
                || folder.contains(['/', '\\', ':'])
            {
                continue;
            }
            let game = library.join("common").join(folder);
            for &directory in game_folders {
                add_directory(&mut roots, game.join(directory));
            }
        }
    }
    roots
}

fn add_directory(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if let Ok(path) = path.canonicalize()
        && path.is_dir()
        && !paths.contains(&path)
    {
        paths.push(path);
    }
}

fn read_config(path: &Path) -> Option<String> {
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= MAX_CONFIG_BYTES).then_some(text)
}

fn parse(text: &str) -> Option<keyvalues::KvDocument<'_>> {
    keyvalues::parse(text.trim_start_matches('\u{feff}'), &Limits::default()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn mounts_installed_source_content_from_a_second_steam_library() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = env::temp_dir().join(format!("surfd-steam-{}-{nonce}", std::process::id()));
        let steam = temp.join("Steam");
        let library = temp.join("Games");
        let steamapps = library.join("steamapps");
        let game = steamapps.join("common/Counter-Strike Source");
        fs::create_dir_all(steam.join("steamapps")).unwrap();
        fs::create_dir_all(game.join("cstrike")).unwrap();
        fs::create_dir_all(game.join("hl2")).unwrap();
        let path = library.to_string_lossy().replace('\\', "\\\\");
        fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(r#""libraryfolders" {{ "1" {{ "path" "{path}" "apps" {{ "240" "1" }} }} }}"#),
        )
        .unwrap();
        fs::write(
            steamapps.join("appmanifest_240.acf"),
            r#""AppState" { "appid" "240" "installdir" "Counter-Strike Source" }"#,
        )
        .unwrap();
        let roots = find_games(&[steam]);
        assert_eq!(
            roots,
            [
                game.join("cstrike").canonicalize().unwrap(),
                game.join("hl2").canonicalize().unwrap()
            ]
        );
        fs::remove_dir_all(temp).unwrap();
    }
}
