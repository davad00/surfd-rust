use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MapMode {
    #[default]
    Surf,
    Bhop,
}

impl MapMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Surf => "surf",
            Self::Bhop => "bhop",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Surf => "Surf",
            Self::Bhop => "Bhop",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [Self::Surf, Self::Bhop]
            .into_iter()
            .find(|mode| mode.as_str().eq_ignore_ascii_case(value))
    }

    pub fn from_map_name(name: &str) -> Self {
        find(name).map_or_else(
            || {
                if name
                    .get(..5)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bhop_"))
                {
                    Self::Bhop
                } else {
                    Self::Surf
                }
            },
            |entry| entry.mode,
        )
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBspEntry {
    pub bsp_sha256: String,
    pub timer_sha256: String,
}

impl SourceBspEntry {
    pub fn board_revision(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"surfd-ranked-source-v1\0");
        digest.update(self.bsp_sha256.as_bytes());
        digest.update(self.timer_sha256.as_bytes());
        format!("{:x}", digest.finalize())
    }

    pub fn bonus_board_revision(&self, number: u16) -> Option<String> {
        let mut timer = [0_u8; 32];
        if number == 0 || self.timer_sha256.len() != 64 || !self.timer_sha256.is_ascii() {
            return None;
        }
        for (index, byte) in timer.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&self.timer_sha256[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(format!(
            "{}:bonus:{number}:{:x}",
            self.board_revision(),
            Sha256::digest(timer)
        ))
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub author: String,
    #[serde(default)]
    pub mode: MapMode,
    pub tier: Option<u8>,
    #[serde(rename = "type")]
    pub layout: String,
    #[serde(default)]
    pub geometry: String,
    pub visual: Option<String>,
    pub bonus_pack: Option<String>,
    pub bonuses: usize,
    pub source_bsp: Option<SourceBspEntry>,
}

pub fn entries() -> &'static [CatalogEntry] {
    static ENTRIES: OnceLock<Vec<CatalogEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        let mut entries: Vec<CatalogEntry> =
            serde_json::from_str(include_str!("../assets/surfd-catalog.json"))
                .expect("valid bundled map catalog");
        let source: Vec<CatalogEntry> =
            serde_json::from_str(include_str!("../assets/ranked-source-catalog.json"))
                .expect("valid bundled Source map catalog");
        entries.extend(source);
        entries
    })
}

pub fn find(id: &str) -> Option<&'static CatalogEntry> {
    entries()
        .iter()
        .find(|entry| entry.id.eq_ignore_ascii_case(id) || entry.slug.eq_ignore_ascii_case(id))
}

pub fn revision(reference: &str) -> Option<&str> {
    let (_, filename) = reference.rsplit_once('/')?;
    let (hash, suffix) = filename.split_once('.')?;
    (suffix == "json.gz" && hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(hash)
}

impl CatalogEntry {
    #[cfg(feature = "records-service")]
    pub(crate) fn native_board_revision(&self) -> Option<String> {
        if let Some(source) = &self.source_bsp {
            return Some(source.board_revision());
        }
        let geometry = revision(&self.geometry)?;
        Some(crate::rules::PhysicsProfile::Surfd.revision(geometry.to_owned()))
    }

    // These exact collision packs opt into the original prop contact rules.
    // Main-board and profile requests can use the revision without loading a map.
    #[cfg(feature = "records-service")]
    pub(crate) fn had_authored_contacts(&self) -> bool {
        matches!(
            revision(&self.geometry),
            Some(
                "be5fca29e28ec475afa682d2501030a6972422f9af71823aa418f0bd3edafc50" // Pantheon
            | "e7b9a51ed8d7e481500b59dcefa0f312e792cf972fc461fcb057ac92d29bd7af" // Cyberwave
            | "e8e86c1814c0e115d3f826f2fd7601003a5d5da466de5901c00988a3166337fa" // Lt Omnific
            | "10289f2938718032a9609b3cdd7ee29e651e39ab8b2cd65a223f704910b0f3c1" // Surreal
            | "05b00c6763ca4b0fc7ccdd28e7c90d71e6487ee4e8c1617ec49b98506c1a5597" // Ravine
            | "621675d9f5fdd5fdb5cf254b55a949e64a32dd6e0b6969828ea4b00dbbe006f9" // Interference
            | "84b5ff21d200fc54beaf3752f2954efe5d56f9966ba3d98f119aeb3aa8de6c60" // Leidenfrost
            | "0d795283c6d7d5f8e52677cc001cd04d2285d76098f9324837a8f69389cbe084" // Kitsune 2
            )
        )
    }
}

#[cfg(any(feature = "client", feature = "records-service"))]
pub mod files {
    use crate::{
        catalog::{find, revision},
        course::Course,
        surfd_map,
    };
    use flate2::read::MultiGzDecoder;
    use sha2::{Digest, Sha256};
    use std::{
        fs::{self, File},
        io::{self, Read, Write},
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    const ORIGIN: &str = "https://surfd-assets.classic-surf.workers.dev";
    const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;
    const MAX_GEOMETRY_BYTES: u64 = 512 * 1024 * 1024;
    static DOWNLOAD_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    pub fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(180)))
            .build()
            .into()
    }

    pub fn verify_geometry(path: &Path, reference: &str) -> Result<String, String> {
        let revision = revision(reference).ok_or("Map catalog is missing a geometry revision")?;
        let filename = format!("{revision}.json.gz");
        verify_asset(path, &filename).map_err(|error| format!("{filename}: {error}"))?;
        Ok(revision.to_owned())
    }

    pub fn load_ranked(root: &Path, map: &str) -> Result<Course, String> {
        if map.eq_ignore_ascii_case("downline") {
            return Ok(Course::practice());
        }
        let entry = find(map).ok_or("Unknown ranked map")?;
        if entry.source_bsp.is_some() {
            let bsp = root.join(format!("{}.bsp", entry.slug));
            let compressed = bsp.with_extension("bsp.gz");
            let path = if bsp.is_file() { bsp } else { compressed };
            return crate::source_bsp::load_ranked_collision(&path, entry);
        }
        let directory = cache_directory(root, &entry.slug);
        fs::create_dir_all(&directory)
            .map_err(|error| format!("Could not create map cache: {error}"))?;
        let agent = agent();
        let path = fetch_asset(&agent, &directory, &entry.geometry)?;
        let revision =
            revision(&entry.geometry).ok_or("Map catalog is missing a geometry revision")?;
        let mut course = surfd_map::load_collision_verified(&path, revision)?;
        if course.name != entry.id {
            return Err("The geometry belongs to a different map".into());
        }
        course.catalog_revision = Some(revision.to_owned());
        if let Some(reference) = &entry.bonus_pack {
            let path = fetch_asset(&agent, &directory, reference)?;
            let revision = crate::catalog::revision(reference)
                .ok_or("Map catalog is missing a bonus revision")?;
            surfd_map::add_bonus_spawns_verified(&mut course, &path, revision)?;
        }
        Ok(course)
    }
    pub fn cache_directory(root: &Path, slug: &str) -> PathBuf {
        root.join(".surfd-cache").join(slug)
    }

    pub fn fetch_asset(
        agent: &ureq::Agent,
        directory: &Path,
        reference: &str,
    ) -> Result<PathBuf, String> {
        let route = reference.strip_prefix(ORIGIN).unwrap_or(reference);
        let filename = route
            .strip_prefix("/packed/")
            .or_else(|| route.strip_prefix("/maps/"))
            .filter(|name| {
                !name.is_empty()
                    && !name.starts_with('.')
                    && name.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
                    })
            })
            .ok_or_else(|| format!("Unsupported map asset URL: {reference}"))?;
        let path = directory.join(filename);
        if path.is_file() {
            return Ok(path);
        }
        let url = format!("{ORIGIN}{route}");
        let mut response = agent
            .get(&url)
            .call()
            .map_err(|error| format!("{filename}: {error}"))?;
        let sequence = DOWNLOAD_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let part = directory.join(format!("{filename}.{}.{sequence}.part", std::process::id()));
        let result = (|| -> io::Result<()> {
            let mut file = File::create(&part)?;
            let mut body = response.body_mut().as_reader().take(MAX_ASSET_BYTES + 1);
            let copied = io::copy(&mut body, &mut file)?;
            if copied == 0 || copied > MAX_ASSET_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected map asset size",
                ));
            }
            file.flush()?;
            drop(file);
            verify_asset(&part, filename)?;
            match fs::rename(&part, &path) {
                Err(_) if path.is_file() => {
                    fs::remove_file(&part).ok();
                    Ok(())
                }
                result => result,
            }
        })();
        if let Err(error) = result {
            fs::remove_file(&part).ok();
            return Err(format!("{filename}: {error}"));
        }
        Ok(path)
    }

    pub fn verify_asset(path: &Path, filename: &str) -> io::Result<()> {
        let Some((hash, extension)) = filename.split_once('.') else {
            return Ok(());
        };
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(());
        }
        let file = File::open(path)?;
        let mut digest = Sha256::new();
        if extension == "json.gz" {
            let expanded = io::copy(
                &mut MultiGzDecoder::new(file).take(MAX_GEOMETRY_BYTES + 1),
                &mut digest,
            )?;
            if expanded > MAX_GEOMETRY_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "map geometry exceeds the size limit",
                ));
            }
        } else {
            io::copy(&mut io::BufReader::new(file), &mut digest)?;
        }
        if !format!("{:x}", digest.finalize()).eq_ignore_ascii_case(hash) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "map asset checksum mismatch",
            ));
        }
        Ok(())
    }
}
