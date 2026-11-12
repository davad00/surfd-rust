use std::{collections::HashSet, fs::File, io::Read, path::Path, sync::OnceLock};

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

#[derive(Clone, Deserialize, Serialize)]
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

#[derive(Clone, Deserialize, Serialize)]
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
    #[serde(default, skip_serializing_if = "is_zero")]
    pub min_source_rules: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<SourceDownload>,
}

pub const SOURCE_MAP_RULES: u8 = 2;
pub(crate) const SOURCE_RULES_NORMALIZED_OUTPUTS: u8 = 2;

fn is_zero(value: &u8) -> bool {
    *value == 0
}

pub(crate) fn valid_source_missing_filters(names: &[String]) -> bool {
    names.len() <= 32
        && names.windows(2).all(|pair| pair[0] < pair[1])
        && names.iter().all(|name| {
            !name.is_empty()
                && name.len() <= 64
                && name.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
        })
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceDownload {
    pub bsp_url: String,
    pub timer_url: String,
    pub compression: MapCompression,
    pub stages: u16,
    pub checkpoints: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shavit: Option<ShavitDownload>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShavitDownload {
    pub sha256: String,
    pub tier: Option<u8>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub spawn_targets: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub gravity_outputs: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_filters: Vec<String>,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MapCompression {
    None,
    Gzip,
    Bzip2,
}

pub const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_COMMUNITY_MAPS: usize = 4_096;
static COMMUNITY: OnceLock<CommunityCatalog> = OnceLock::new();
static ENTRIES: OnceLock<Vec<CatalogEntry>> = OnceLock::new();

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommunityCatalog {
    pub version: u8,
    pub maps: Vec<CatalogEntry>,
    #[serde(skip)]
    new_maps: HashSet<String>,
}

pub fn community_catalog() -> Option<&'static CommunityCatalog> {
    COMMUNITY.get()
}

pub(crate) fn is_community_map(id: &str) -> bool {
    let Some(catalog) = community_catalog() else {
        return false;
    };
    catalog.new_maps.contains(id)
}

pub fn configure_from_env() -> Result<(), String> {
    if let Some(path) = std::env::var_os("SURF_MAP_CATALOG") {
        return configure_community(&read_catalog(Path::new(&path))?);
    }
    Ok(())
}

pub fn read_catalog(path: &Path) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    File::open(path)
        .and_then(|file| {
            file.take(MAX_CATALOG_BYTES as u64 + 1)
                .read_to_end(&mut data)
        })
        .map_err(|error| format!("Could not read map catalog: {error}"))?;
    Ok(data)
}

#[cfg(feature = "client")]
pub fn cache_catalog(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!("{}.{}.part", std::process::id(), sequence));
    let result = std::fs::write(&temporary, bytes).and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err() {
        std::fs::remove_file(temporary).ok();
    }
    result
}

pub fn configure_community(data: &[u8]) -> Result<(), String> {
    if data.len() > MAX_CATALOG_BYTES {
        return Err("Community map catalog exceeds 2 MiB".into());
    }
    let mut catalog: CommunityCatalog = serde_json::from_slice(data)
        .map_err(|error| format!("Invalid community map catalog: {error}"))?;
    if catalog.version != 1 || catalog.maps.len() > MAX_COMMUNITY_MAPS {
        return Err("Unsupported community map catalog version or size".into());
    }
    let base = bundled_entries();
    let mut ids = HashSet::new();
    for entry in &catalog.maps {
        let valid_hash = |hash: &str| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        let valid_id = |id: &str| {
            !id.is_empty()
                && id.len() <= 64
                && id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        };
        let (Some(source), Some(download)) = (&entry.source_bsp, &entry.download) else {
            return Err(format!(
                "{} has no pinned Source map and timer downloads",
                entry.id
            ));
        };
        if !valid_id(&entry.id)
            || entry.id != entry.slug
            || entry.id == "downline"
            || !ids.insert(entry.id.as_str())
            || entry.name.is_empty()
            || entry.name.len() > 120
            || entry.author.is_empty()
            || entry.author.len() > 200
            || !entry.tier.is_some_and(|tier| tier > 0)
            || entry.min_source_rules > SOURCE_MAP_RULES
            || entry.layout
                != if download.stages > 1 {
                    "staged"
                } else {
                    "linear"
                }
            || !entry.geometry.is_empty()
            || entry.visual.is_some()
            || entry.bonus_pack.is_some()
            || !valid_hash(&source.bsp_sha256)
            || !valid_hash(&source.timer_sha256)
            || !valid_download_url(&download.bsp_url)
            || !valid_download_url(&download.timer_url)
            || download.shavit.as_ref().is_some_and(|shavit| {
                !valid_hash(&shavit.sha256)
                    || shavit.tier == Some(0)
                    || shavit.spawn_targets.len() > 128
                    || shavit.spawn_targets.iter().any(|(key, target)| {
                        key.len() > 32 || target.is_empty() || target.len() > 128
                    })
                    || !valid_source_missing_filters(&shavit.missing_filters)
                    || ((shavit.gravity_outputs || !shavit.missing_filters.is_empty())
                        && entry.min_source_rules < SOURCE_RULES_NORMALIZED_OUTPUTS)
            })
        {
            return Err(format!("Invalid community map entry: {}", entry.id));
        }
        if let Some(known) = base
            .iter()
            .find(|known| known.id == entry.id || known.slug == entry.id)
        {
            if !(known.id == entry.id
                && known.mode == entry.mode
                && known.tier == entry.tier
                && known.bonuses == entry.bonuses
                && known.source_bsp.as_ref().is_some_and(|pin| {
                    pin.bsp_sha256 == source.bsp_sha256 && pin.timer_sha256 == source.timer_sha256
                }))
            {
                return Err(format!(
                    "{} conflicts with a built-in map revision",
                    entry.id
                ));
            }
        } else {
            catalog.new_maps.insert(entry.id.clone());
        }
    }
    if ENTRIES.get().is_some() || COMMUNITY.set(catalog).is_err() {
        return Err("The map catalog has already been initialized".into());
    }
    Ok(())
}

fn valid_download_url(url: &str) -> bool {
    let Some((host, path)) = url
        .strip_prefix("https://")
        .and_then(|url| url.split_once('/'))
    else {
        return false;
    };
    !host.is_empty()
        && host.contains('.')
        && host.parse::<std::net::IpAddr>().is_err()
        && !host.to_ascii_lowercase().ends_with(".localhost")
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        && !path.is_empty()
        && url.len() <= 2048
        && url
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'\\' | b'#'))
}

fn bundled_entries() -> Vec<CatalogEntry> {
    let mut entries: Vec<CatalogEntry> =
        serde_json::from_str(include_str!("../assets/surfd-catalog.json"))
            .expect("valid bundled map catalog");
    let source: Vec<CatalogEntry> =
        serde_json::from_str(include_str!("../assets/ranked-source-catalog.json"))
            .expect("valid bundled Source map catalog");
    entries.extend(source);
    entries
}

pub fn entries() -> &'static [CatalogEntry] {
    ENTRIES.get_or_init(|| {
        let mut entries = bundled_entries();
        if let Some(catalog) = community_catalog() {
            for map in &catalog.maps {
                if let Some(existing) = entries.iter_mut().find(|entry| entry.id == map.id) {
                    *existing = map.clone();
                } else {
                    entries.push(map.clone());
                }
            }
        }
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
        catalog::{CatalogEntry, MapCompression, find, revision},
        course::Course,
        session::Session,
        surfd_map,
    };
    use flate2::read::MultiGzDecoder;
    use sha2::{Digest, Sha256};
    use std::{
        fs::{self, File},
        io::{self, BufReader, Read, Write},
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    const ORIGIN: &str = "https://surfd-assets.classic-surf.workers.dev";
    const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;
    const MAX_GEOMETRY_BYTES: u64 = 512 * 1024 * 1024;
    const MAX_SOURCE_TIMER_BYTES: u64 = 2 * 1024 * 1024;
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
            return load_source(root, entry, false);
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

    pub fn load_source(root: &Path, entry: &CatalogEntry, visuals: bool) -> Result<Course, String> {
        let raw = root.join(format!("{}.bsp", entry.slug));
        let zipped = raw.with_extension("bsp.gz");
        let local = if raw.is_file() { raw } else { zipped };
        let load = |path: &Path| {
            if visuals {
                crate::source_bsp::load_ranked(path, entry)
            } else {
                crate::source_bsp::load_ranked_collision(path, entry)
            }
        };
        let Some(download) = &entry.download else {
            return load(&local);
        };
        let source = entry
            .source_bsp
            .as_ref()
            .ok_or("Missing ranked map hashes")?;
        let course = match local.is_file().then(|| load(&local)) {
            Some(Ok(course)) => course,
            _ => {
                let directory = cache_directory(root, &entry.slug).join(source.board_revision());
                fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
                let path = directory.join(format!("{}.bsp", entry.slug));
                match path.is_file().then(|| load(&path)) {
                    Some(Ok(course)) => course,
                    _ => {
                        fetch_source(entry, &path)?;
                        load(&path)?
                    }
                }
            }
        };
        let session = Session::new(&course);
        let stages = session.stage_count(&course).unwrap_or(0);
        if stages != usize::from(download.stages)
            || session.checkpoint_count() != usize::from(download.checkpoints)
            || course.bonus_routes.len() != entry.bonuses
            || !(1..=entry.bonuses).all(|number| course.bonus(number as u16).is_some())
            || !crate::local_map::has_start_and_finish(&course)
        {
            return Err(format!(
                "{} does not match its published timing layout",
                entry.id
            ));
        }
        Ok(course)
    }

    fn fetch_source(entry: &CatalogEntry, path: &Path) -> Result<(), String> {
        let source = entry
            .source_bsp
            .as_ref()
            .ok_or("Missing ranked map hashes")?;
        let download = entry.download.as_ref().ok_or("Missing map download")?;
        let timer = path.with_extension("surf.json");
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(180)))
            .max_redirects(0)
            .https_only(true)
            .build()
            .into();
        download_source_file(
            &agent,
            path,
            &download.bsp_url,
            &source.bsp_sha256,
            download.compression,
            MAX_GEOMETRY_BYTES,
        )?;
        if let Some(shavit) = &download.shavit {
            if verify_sha256(&timer, &source.timer_sha256, MAX_SOURCE_TIMER_BYTES).is_ok() {
                return Ok(());
            }
            let original = path.with_extension("shavit.json");
            download_source_file(
                &agent,
                &original,
                &download.timer_url,
                &shavit.sha256,
                MapCompression::None,
                MAX_SOURCE_TIMER_BYTES,
            )?;
            let raw = fs::read(&original).map_err(|error| error.to_string())?;
            let normalized = crate::shavit_download::normalize(
                &raw,
                &source.bsp_sha256,
                entry.mode,
                shavit.tier,
                &shavit.spawn_targets,
                shavit.gravity_outputs,
                &shavit.missing_filters,
            )?;
            return write_source_file(
                &timer,
                normalized.as_slice(),
                &source.timer_sha256,
                MAX_SOURCE_TIMER_BYTES,
            );
        }
        download_source_file(
            &agent,
            &timer,
            &download.timer_url,
            &source.timer_sha256,
            MapCompression::None,
            MAX_SOURCE_TIMER_BYTES,
        )
    }

    fn download_source_file(
        agent: &ureq::Agent,
        destination: &Path,
        url: &str,
        expected: &str,
        compression: MapCompression,
        limit: u64,
    ) -> Result<(), String> {
        if destination.is_file() && verify_sha256(destination, expected, limit).is_ok() {
            return Ok(());
        }
        let mut response = agent.get(url).call().map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!("Map download returned HTTP {}", response.status()));
        }
        let body = response.body_mut().as_reader().take(limit + 1);
        match compression {
            MapCompression::None => write_source_file(destination, body, expected, limit),
            MapCompression::Gzip => {
                write_source_file(destination, MultiGzDecoder::new(body), expected, limit)
            }
            MapCompression::Bzip2 => write_source_file(
                destination,
                bzip2::read::MultiBzDecoder::new(body),
                expected,
                limit,
            ),
        }
    }

    fn write_source_file(
        destination: &Path,
        reader: impl Read,
        expected: &str,
        limit: u64,
    ) -> Result<(), String> {
        let name = destination.file_name().ok_or("Missing map filename")?;
        let sequence = DOWNLOAD_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let part = destination.with_extension(format!("{}.{}.part", std::process::id(), sequence));
        let result = (|| -> io::Result<()> {
            let mut file = File::create(&part)?;
            let size = io::copy(&mut reader.take(limit + 1), &mut file)?;
            if size == 0 || size > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected map file size",
                ));
            }
            file.flush()?;
            drop(file);
            verify_sha256(&part, expected, limit)?;
            match fs::rename(&part, destination) {
                Ok(()) => Ok(()),
                Err(error) => verify_sha256(destination, expected, limit).map_err(|_| error),
            }
        })();
        if part.is_file() {
            fs::remove_file(&part).ok();
        }
        result.map_err(|error| format!("{}: {error}", name.to_string_lossy()))
    }

    fn verify_sha256(path: &Path, expected: &str, limit: u64) -> io::Result<()> {
        let mut digest = Sha256::new();
        let bytes = io::copy(
            &mut BufReader::new(File::open(path)?).take(limit + 1),
            &mut digest,
        )?;
        if bytes == 0 || bytes > limit || format!("{:x}", digest.finalize()) != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "map file checksum mismatch",
            ));
        }
        Ok(())
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
