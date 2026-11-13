#[cfg(feature = "client-audio")]
mod ambient;
mod bench;
mod bench_render_trace;
pub(crate) use bevy_surf::loadout as cosmetics;
mod flythrough;
mod knife_asset;
mod library;
mod menu;
mod online;
mod online_ui;
mod online_world;
mod records_client;
mod room_check;
mod surfd_history;
mod ui_font;
mod view;
mod world;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use bevy_surf::{
    catalog::{self, CatalogEntry, files},
    course::Course,
    local_map,
    record_protocol::MAX_REPLAY_BYTES,
    reference_ghost::{MAX_TRACK_BYTES, ReferenceGhost},
    replay::RunReplay,
    surfd_map,
};
use library::MapLibrary;
use serde::Deserialize;
use view::StartupReplay;

const USAGE: &str = "Usage: bevy-surf [--map FILE.bsp | --inspect FILE.bsp | --catalog MAP | --inspect-catalog MAP | --replay FILE.json | --inspect-replay FILE.json | --bench-replay FILE.json | --bench-map FILE.bsp | --surfd GEOMETRY [VISUAL-MANIFEST] | --inspect-surfd GEOMETRY [VISUAL-MANIFEST] | --room-status ROOM_ID | --check-room ROOM_ID]";

fn main() -> ExitCode {
    match launch() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn launch() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let flag = args.next();
    if std::env::var_os("SURF_MAP_CATALOG").is_some() {
        catalog::configure_from_env()?;
    } else if flag
        .as_deref()
        .and_then(|arg| arg.to_str())
        .is_none_or(|flag| {
            matches!(
                flag,
                "--catalog"
                    | "--inspect-catalog"
                    | "--replay"
                    | "--inspect-replay"
                    | "--bench-replay"
                    | "--bench-map"
                    | "--check-room"
            )
        })
    {
        let directory = library::data_directory();
        let cache = directory.join(records_client::community_catalog_cache_name());
        match records_client::community_catalog().and_then(|data| {
            catalog::configure_community(&data)?;
            Ok(data)
        }) {
            Ok(data) => {
                if let Err(error) = catalog::cache_catalog(&cache, &data) {
                    eprintln!("Could not cache community maps: {error}");
                }
            }
            Err(error) => {
                if cache.is_file() {
                    if let Err(cache_error) = catalog::read_catalog(&cache)
                        .and_then(|data| catalog::configure_community(&data))
                    {
                        eprintln!("Could not read cached community maps: {cache_error}");
                    }
                }
                eprintln!("Could not refresh community maps: {error}");
            }
        }
    }
    let mut playback = None;
    let course = match flag.as_deref().and_then(|arg| arg.to_str()) {
        None if flag.is_none() => Course::practice(),
        Some("--help" | "-h") => {
            println!("{USAGE}");
            return Ok(());
        }
        Some(command @ ("--room-status" | "--check-room")) => {
            let room = args
                .next()
                .and_then(|arg| arg.into_string().ok())
                .ok_or("Expected a managed room ID")?;
            if args.next().is_some() {
                return Err("Expected a single managed room ID".into());
            }
            return if command == "--room-status" {
                room_check::status(&room)
            } else {
                room_check::run(&room)
            };
        }
        Some("--map" | "--inspect") => {
            let path = PathBuf::from(
                args.next()
                    .ok_or("Expected a .bsp, .bsp.gz, or .pk3 path")?,
            );
            if args.next().is_some() {
                return Err("Expected a single map path".to_owned());
            }
            let course = local_map::load(&path)?;
            if flag.as_deref().is_some_and(|arg| arg == "--inspect") {
                describe(&course);
                return Ok(());
            }
            course
        }
        Some("--surfd" | "--inspect-surfd") => {
            let path = PathBuf::from(args.next().ok_or("Expected the Surfd geometry file")?);
            let manifest = args.next().map(PathBuf::from);
            if args.next().is_some() {
                return Err(USAGE.to_owned());
            }
            let course = surfd_map::load(&path, manifest.as_deref())?;
            if flag.as_deref().is_some_and(|arg| arg == "--inspect-surfd") {
                describe(&course);
                return Ok(());
            }
            course
        }
        Some("--catalog" | "--inspect-catalog") => {
            let map = args
                .next()
                .and_then(|arg| arg.into_string().ok())
                .ok_or("Expected a catalog map name")?;
            if args.next().is_some() {
                return Err(USAGE.to_owned());
            }
            let library = MapLibrary::read();
            let source = library
                .catalog_source(&map)
                .ok_or_else(|| format!("Unknown catalog map: {map}"))?;
            let course = source.load(&library.directory, |status| eprintln!("{status}"))?;
            if flag
                .as_deref()
                .is_some_and(|arg| arg == "--inspect-catalog")
            {
                describe(&course);
                return Ok(());
            }
            course
        }
        Some("--bench-map") => {
            let path = PathBuf::from(args.next().ok_or("Expected a map file")?);
            if args.next().is_some() {
                return Err(USAGE.to_owned());
            }
            return view::run(
                Course::practice(),
                None,
                Some(bench::BenchRequest {
                    source: library::MapSource::Source(path),
                    replay: None,
                }),
            );
        }
        Some("--replay" | "--inspect-replay" | "--bench-replay") => {
            let path = PathBuf::from(args.next().ok_or("Expected a replay file")?);
            if args.next().is_some() {
                return Err(USAGE.to_owned());
            }
            if flag.as_deref().is_some_and(|arg| arg == "--bench-replay") {
                let library = MapLibrary::read();
                let (map, replay) = read_replay(&path, &library)?;
                let source = if map.eq_ignore_ascii_case("downline") {
                    library::MapSource::Downline
                } else {
                    library
                        .catalog_source(&map)
                        .ok_or("Benchmark replay map is outside the catalog")?
                };
                return view::run(
                    Course::practice(),
                    None,
                    Some(bench::BenchRequest {
                        source,
                        replay: Some(replay),
                    }),
                );
            }
            let inspect = flag.as_deref().is_some_and(|arg| arg == "--inspect-replay");
            let (course, replay) = load_replay(&path, inspect)?;
            if inspect {
                inspect_replay(&course, replay)?;
                return Ok(());
            }
            playback = Some(replay);
            course
        }
        _ => return Err(USAGE.to_owned()),
    };
    view::run(course, playback, None)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplayHeader {
    map: Option<String>,
    map_id: Option<String>,
    map_version: Option<String>,
    format: Option<u8>,
}

fn load_replay(path: &Path, inspect: bool) -> Result<(Course, StartupReplay), String> {
    let library = MapLibrary::read();
    let (map, replay) = read_replay(path, &library)?;
    let course = load_replay_course(&library, &map, inspect)?;
    if let StartupReplay::Reference(track) = &replay
        && !track.matches_course(&course)
    {
        return Err("The reference replay belongs to a different course revision".into());
    }
    Ok((course, replay))
}

fn read_replay(path: &Path, library: &MapLibrary) -> Result<(String, StartupReplay), String> {
    use std::io::Read;
    let file =
        std::fs::File::open(path).map_err(|error| format!("Could not open replay: {error}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_TRACK_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_TRACK_BYTES {
        return Err("Replay is too large".into());
    }
    let header: ReplayHeader =
        serde_json::from_slice(&bytes).map_err(|error| format!("Invalid replay: {error}"))?;
    if let Some(map) = header.map {
        if bytes.len() > MAX_REPLAY_BYTES {
            return Err("Native replay is too large".into());
        }
        let replay: RunReplay = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid native replay: {error}"))?;
        return Ok((map, StartupReplay::Native(replay)));
    }
    let map = header
        .map_id
        .ok_or("Replay is missing its map identifier")?;
    let entry = catalog::find(&map).ok_or("Reference replay map is outside the catalog")?;
    let track = if header.format.is_some() {
        let version = header
            .map_version
            .ok_or("Reference replay is missing its map revision")?;
        let bonus = ReferenceGhost::bonus_in_version(&version)?;
        let assets = reference_replay_assets(&library.directory, entry, bonus)?;
        import_reference_replay(path, &assets)?
    } else {
        ReferenceGhost::decode(&bytes)?
    };
    if track.recorded_splits().is_none() {
        return Err("This file contains a race ghost. Import the command replay to watch it in first person.".into());
    }
    Ok((entry.id.clone(), StartupReplay::Reference(track)))
}

fn load_replay_course(library: &MapLibrary, map: &str, inspect: bool) -> Result<Course, String> {
    if map.eq_ignore_ascii_case("downline") {
        Ok(Course::practice())
    } else if inspect {
        files::load_ranked(&library.directory, map)
    } else {
        library
            .catalog_source(map)
            .ok_or("Replay map is outside the catalog")?
            .load(&library.directory, |status| eprintln!("{status}"))
    }
}

struct ReferenceReplayAssets {
    geometry: PathBuf,
    bonus_pack: Option<PathBuf>,
}

fn reference_replay_assets(
    root: &Path,
    entry: &CatalogEntry,
    bonus: Option<u16>,
) -> Result<ReferenceReplayAssets, String> {
    let directory = files::cache_directory(root, &entry.slug);
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create map cache: {error}"))?;
    let agent = files::agent();
    let geometry = files::fetch_asset(&agent, &directory, &entry.geometry)?;
    files::verify_geometry(&geometry, &entry.geometry)?;
    let bonus_pack = if bonus.is_some() {
        let reference = entry
            .bonus_pack
            .as_deref()
            .ok_or("This map has no original bonus pack")?;
        let path = files::fetch_asset(&agent, &directory, reference)?;
        files::verify_geometry(&path, reference)?;
        Some(path)
    } else {
        None
    };
    Ok(ReferenceReplayAssets {
        geometry,
        bonus_pack,
    })
}

fn import_reference_replay(
    path: &Path,
    assets: &ReferenceReplayAssets,
) -> Result<ReferenceGhost, String> {
    let script = std::env::var_os("SURF_REFERENCE_IMPORTER")
        .map(PathBuf::from)
        .or_else(|| reference_tool("import-reference-replay.ts"))
        .ok_or("Open a converted Surfd track or use the source repository's replay tools.")?;
    let mut command = Command::new("bun");
    command.arg(script).arg(path).arg(&assets.geometry);
    if let Some(pack) = &assets.bonus_pack {
        command.arg("--bonus-pack").arg(pack);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command
        .output()
        .map_err(|error| format!("Could not start the local Bun replay importer: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Reference replay import failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    ReferenceGhost::decode(&output.stdout)
}

fn reference_tool(name: &str) -> Option<PathBuf> {
    let executable_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    std::env::current_dir()
        .ok()
        .into_iter()
        .chain(executable_directory)
        .map(|directory| directory.join("tools").join(name))
        .find(|tool| tool.is_file())
}

fn inspect_replay(course: &Course, replay: StartupReplay) -> Result<(), String> {
    use bevy_surf::{replay::Playback, session::RunPhase};
    match replay {
        StartupReplay::Native(replay) => {
            let mut playback = Playback::new(course, replay).map_err(|error| error.to_string())?;
            while playback.tick(course) {}
            println!(
                "{}: {} commands, {:?}, {:.9} s, splits {:?}",
                course.name,
                playback.cursor,
                playback.session.phase,
                playback.session.elapsed,
                playback.session.splits
            );
            if playback.session.phase != RunPhase::Finished {
                return Err("Replay did not complete the route".into());
            }
        }
        StartupReplay::Reference(track) => {
            let (splits, _) = track.recorded_splits().ok_or("Missing recorded splits")?;
            let route = track.bonus().map_or_else(
                || course.name.clone(),
                |number| format!("{} Bonus {number}", course.name),
            );
            println!(
                "{}: {} commands, Finished, {:.9} s, splits {splits:?}",
                route,
                track.sample_count() - 1,
                track.duration()
            );
        }
    }
    Ok(())
}

fn describe(course: &Course) {
    println!(
        "{}: {} polygons, {} indexed meshes, {} textures, {} collision brushes, {} zones, {} practice starts",
        course.name,
        course.surfaces.len(),
        course.visual_meshes.len(),
        course.textures.len(),
        course.brushes.len(),
        course.zones.len(),
        course.practice_spawns.len(),
    );
    let session = bevy_surf::session::Session::new(course);
    if let Some(mode) = course.imported_source_mode().or_else(|| {
        (course.catalog_revision.is_none() && session.phase == bevy_surf::session::RunPhase::Ready)
            .then(|| course.map_mode().as_str())
    }) {
        println!(
            "Local {mode} timer: {} main checkpoints, {} bonuses, max velocity {}",
            session.checkpoint_count(),
            course.bonus_routes.len(),
            bevy_surf::rules::MovementRules::for_course(course).max_velocity,
        );
    }
}
