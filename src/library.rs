use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

use bevy::prelude::*;
use bevy_surf::catalog::{
    self, CatalogEntry, MapMode,
    files::{agent, cache_directory, fetch_asset},
};
use bevy_surf::{
    course::Course,
    local_map,
    session::Session,
    source_bsp,
    surfd_map::{self, VisualLoadOptions},
    tutorial,
};

pub const STARTER_MAP: &str = "surf_kitsune";

fn environment_directory(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn data_directory() -> PathBuf {
    environment_directory("SURF_DATA").unwrap_or_else(default_data_directory)
}

fn default_data_directory() -> PathBuf {
    #[cfg(windows)]
    let base = environment_directory("LOCALAPPDATA").or_else(|| {
        environment_directory("USERPROFILE").map(|home| home.join("AppData").join("Local"))
    });
    #[cfg(target_os = "macos")]
    let base = environment_directory("HOME").map(|home| home.join("Library/Application Support"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = environment_directory("XDG_DATA_HOME")
        .filter(|path| path.is_absolute())
        .or_else(|| environment_directory("HOME").map(|home| home.join(".local/share")));

    base.map_or_else(
        || PathBuf::from("maps"),
        |base| base.join("Surfd Rust").join("maps"),
    )
}

pub(crate) fn reference_preview(slug: &str) -> Option<PathBuf> {
    environment_directory("SURF_REFERENCE_MIRROR")
        .map(|mirror| mirror.join("previews").join(format!("{slug}.jpg")))
}

#[derive(Clone)]
pub enum MapSource {
    Downline,
    Tutorial(&'static str),
    Source(PathBuf),
    PracticeBsp(Arc<CatalogEntry>),
    RankedBsp {
        map: String,
        path: Option<PathBuf>,
    },
    Catalog {
        slug: String,
        geometry: String,
        visual: Option<String>,
        bonus_pack: Option<String>,
    },
}

impl MapSource {
    pub fn load(self, directory: &Path, status: impl Fn(String)) -> Result<Course, String> {
        self.load_with_visual_options(directory, VisualLoadOptions::default(), status)
    }

    fn load_with_visual_options(
        self,
        directory: &Path,
        options: VisualLoadOptions,
        status: impl Fn(String),
    ) -> Result<Course, String> {
        match self {
            Self::Downline => Ok(Course::practice()),
            Self::Tutorial(id) => {
                tutorial::course(id).ok_or_else(|| format!("Unknown lesson: {id}"))
            }
            Self::Source(path) => {
                let course = local_map::load(&path)?;
                if !local_map::has_start_and_finish(&course) {
                    return Err("This map has no verified start and finish zones.".into());
                }
                Ok(course)
            }
            Self::PracticeBsp(entry) => {
                status(format!("Loading {} and its verified timer…", entry.name));
                let mut course =
                    catalog::files::load_source(&directory.join("practice-cache"), &entry, true)?;
                course.catalog_revision = None;
                Ok(course)
            }
            Self::RankedBsp { map, path } => {
                let entry = catalog::find(&map).ok_or("Unknown ranked Source map")?;
                if let Some(path) = path {
                    return source_bsp::load_ranked(&path, entry);
                }
                status(format!("Loading {} and its verified timer…", entry.name));
                catalog::files::load_source(directory, entry, true)
            }
            Self::Catalog {
                slug,
                geometry,
                visual,
                bonus_pack,
            } => load_catalog(
                directory,
                &slug,
                &geometry,
                visual.as_deref(),
                bonus_pack.as_deref(),
                options,
                status,
            ),
        }
    }
}

pub struct MapEntry {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub mode: MapMode,
    pub tier: Option<u8>,
    pub source: MapSource,
}

#[cfg(test)]
mod local_map_tests {
    use super::*;

    #[test]
    fn practice_manifest_entries_cannot_replace_ranked_maps_or_join_rooms() {
        let manifest = serde_json::json!({
            "version": 1,
            "maps": ["surf_kitsune", "bhop_practice_catalog_fixture"].map(|id| serde_json::json!({
                "id": id,
                "slug": id,
                "name": id,
                "author": "Map author",
                "mode": "bhop",
                "tier": 2,
                "type": "linear",
                "bonuses": 0,
                "sourceBsp": {"bspSha256": "a".repeat(64), "timerSha256": "b".repeat(64)},
                "download": {
                    "bspUrl": "https://example.com/map.bsp.bz2",
                    "timerUrl": "https://example.com/map.surf.json",
                    "compression": "bzip2",
                    "stages": 0,
                    "checkpoints": 0
                }
            }))
        });
        let mut library = MapLibrary::read_with_roots(PathBuf::new(), []);
        library
            .add_practice_catalog(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();

        let practice = library
            .entries
            .iter()
            .filter(|map| map.is_practice_download())
            .collect::<Vec<_>>();
        assert_eq!(practice.len(), 1);
        assert_eq!(practice[0].id, "practice:bhop_practice_catalog_fixture");
        assert_eq!(practice[0].mode, MapMode::Bhop);
        assert_eq!(practice[0].tier, Some(2));
        assert!(catalog::find("bhop_practice_catalog_fixture").is_none());
        assert!(
            library
                .catalog_source("bhop_practice_catalog_fixture")
                .is_none()
        );
        for managed in [true, false] {
            assert!(
                library
                    .room_source("bhop_practice_catalog_fixture", MapMode::Bhop, managed)
                    .is_none()
            );
        }
        assert!(library.catalog_source("surf_kitsune").is_some());
    }

    #[test]
    fn reads_local_maps_from_distinct_roots_without_shadowing_catalog() {
        use sha2::{Digest, Sha256};

        let root = std::env::temp_dir().join(format!("surfd-local-maps-{}", std::process::id()));
        let portable = root.join("portable");
        let bsp = b"VBSP\x14\0\0\0";
        let timer = |mode: &str, bytes: &[u8]| {
            let zones = ["start", "end"].map(|kind| {
                serde_json::json!({
                    "kind": kind, "points": [[0, 0], [64, 0], [0, 64]], "bottom": 0, "height": 64
                })
            });
            serde_json::json!({
                "version": 1, "timer": "shavit", "mode": mode, "bsp_sha256": format!("{:x}", Sha256::digest(bytes)),
                "tracks": [{ "id": 0, "checkpoints_required": false, "checkpoints_ordered": true,
                    "limit_start_ground_speed": false, "bhop_enabled": false }],
                "zones": zones
            })
            .to_string()
        };
        fs::create_dir_all(&portable).unwrap();
        fs::write(root.join("surf_kitsune.bsp"), bsp).unwrap();
        fs::write(
            root.join("surf_kitsune.metadata.json"),
            br#"{"mode":"surf","tier":1}"#,
        )
        .unwrap();
        fs::write(root.join("surf_kitsune.surf.json"), timer("surf", bsp)).unwrap();
        fs::write(portable.join("bhop_easy.BSP"), bsp).unwrap();
        fs::write(
            portable.join("bhop_easy.metadata.json"),
            br#"{"mode":"bhop","tier":1}"#,
        )
        .unwrap();
        fs::write(portable.join("bhop_easy.surf.json"), timer("bhop", bsp)).unwrap();
        fs::write(portable.join("surf_untiered.bsp"), bsp).unwrap();
        fs::write(portable.join("surf_untiered.surf.json"), timer("surf", bsp)).unwrap();
        fs::write(portable.join("bhop_untimed.bsp"), bsp).unwrap();
        fs::write(
            portable.join("bhop_untimed.metadata.json"),
            br#"{"mode":"bhop","tier":1,"timed":true}"#,
        )
        .unwrap();
        fs::write(
            portable.join("bhop_untimed.surf.json"),
            timer("bhop", b"different BSP"),
        )
        .unwrap();

        let library = MapLibrary::read_with_roots(
            root.clone(),
            [root.clone(), root.join("."), portable.clone()],
        );
        let local = library
            .entries
            .iter()
            .filter(|entry| entry.is_local())
            .collect::<Vec<_>>();
        assert_eq!(local.len(), 2);
        assert!(local.iter().any(|entry| entry.name == "bhop_easy"));
        assert!(local.iter().any(|entry| entry.id == "local:surf_kitsune"));
        assert!(matches!(
            library.catalog_source("surf_kitsune"),
            Some(MapSource::Catalog { .. })
        ));
        fs::remove_file(portable.join("bhop_easy.surf.json")).unwrap();
        fs::remove_file(portable.join("bhop_easy.metadata.json")).unwrap();
        fs::remove_file(portable.join("surf_untiered.bsp")).unwrap();
        fs::remove_file(portable.join("surf_untiered.surf.json")).unwrap();
        fs::remove_file(portable.join("bhop_untimed.bsp")).unwrap();
        fs::remove_file(portable.join("bhop_untimed.metadata.json")).unwrap();
        fs::remove_file(portable.join("bhop_untimed.surf.json")).unwrap();
        fs::remove_file(portable.join("bhop_easy.BSP")).unwrap();
        fs::remove_file(portable.join("bhop_easy.BSP.timer-check.json")).unwrap();
        fs::remove_file(root.join("surf_kitsune.metadata.json")).unwrap();
        fs::remove_file(root.join("surf_kitsune.surf.json")).unwrap();
        fs::remove_file(root.join("surf_kitsune.bsp")).unwrap();
        fs::remove_file(root.join("surf_kitsune.bsp.timer-check.json")).unwrap();
        fs::remove_dir(portable).unwrap();
        fs::remove_dir(root).unwrap();
    }
}

impl MapEntry {
    pub fn is_local(&self) -> bool {
        matches!(&self.source, MapSource::Source(_))
    }

    pub fn is_practice_download(&self) -> bool {
        matches!(&self.source, MapSource::PracticeBsp(_))
    }

    pub fn preview(&self) -> Option<PathBuf> {
        let MapSource::Catalog { slug, .. } = &self.source else {
            return None;
        };
        reference_preview(slug)
    }

    pub fn status(&self, directory: &Path) -> &'static str {
        match &self.source {
            MapSource::Downline | MapSource::Tutorial(_) => "BUILT IN",
            MapSource::Source(_) => "LOCAL MAP",
            MapSource::PracticeBsp(_) => "PRACTICE ONLY",
            MapSource::RankedBsp { .. } => "RANKED BSP",
            MapSource::Catalog { slug, .. }
                if cache_directory(directory, slug).join("ready").is_file() =>
            {
                "CACHED"
            }
            MapSource::Catalog { .. } => "ON DEMAND",
        }
    }
}

#[derive(Resource)]
pub struct MapLibrary {
    pub entries: Vec<MapEntry>,
    pub directory: PathBuf,
    use_bc: bool,
}

impl MapLibrary {
    pub fn set_compressed_textures(&mut self, enabled: bool) {
        self.use_bc = enabled;
    }

    pub fn rescan(&mut self) {
        let use_bc = self.use_bc;
        *self = Self::read();
        self.use_bc = use_bc;
    }

    pub fn start_load(&self, source: MapSource) -> Result<LoadJob, String> {
        LoadJob::start(self.directory.clone(), source, self.visual_options(), false)
    }

    pub fn start_benchmark_load(
        &self,
        source: MapSource,
        parallel_packs: bool,
    ) -> Result<LoadJob, String> {
        LoadJob::start(
            self.directory.clone(),
            source,
            VisualLoadOptions {
                parallel_packs,
                ..self.visual_options()
            },
            false,
        )
    }

    pub fn start_path_course(&self, id: &str) -> Result<LoadJob, String> {
        let source = self
            .catalog_source(id)
            .ok_or("This course is unavailable.")?;
        LoadJob::start(
            self.directory.clone(),
            source,
            self.visual_options(),
            id == STARTER_MAP,
        )
    }

    fn visual_options(&self) -> VisualLoadOptions {
        VisualLoadOptions {
            use_bc: self.use_bc,
            ..VisualLoadOptions::default()
        }
    }

    pub fn read() -> Self {
        let directory = data_directory();
        let practice_catalog = environment_directory("SURF_PRACTICE_CATALOG").or_else(|| {
            Some(directory.join("practice-catalog.json")).filter(|path| path.is_file())
        });
        let mut roots = vec![directory.clone()];
        if let Ok(exe) = std::env::current_exe()
            && let Some(parent) = exe.parent()
        {
            roots.push(parent.join("maps"));
        }
        if let Some(configured) = std::env::var_os("SURF_MAPS") {
            roots.extend(std::env::split_paths(&configured));
        }
        let mut library = Self::read_with_roots(directory, roots);
        if let Some(path) = practice_catalog
            && let Err(error) =
                catalog::read_catalog(&path).and_then(|bytes| library.add_practice_catalog(&bytes))
        {
            warn!(
                "Could not open practice map catalog {}: {error}",
                path.display()
            );
        }
        library
    }

    fn add_practice_catalog(&mut self, bytes: &[u8]) -> Result<(), String> {
        let practice = catalog::parse_source_catalog(bytes)?;
        self.entries.extend(
            practice
                .maps
                .into_iter()
                .filter(|map| catalog::find(&map.id).is_none())
                .map(|map| MapEntry {
                    id: format!("practice:{}", map.id),
                    name: map.name.clone(),
                    detail: format!("{} · {} · Practice movement", map.author, map.layout),
                    mode: map.mode,
                    tier: map.tier,
                    source: MapSource::PracticeBsp(Arc::new(map)),
                }),
        );
        Ok(())
    }

    fn read_with_roots(directory: PathBuf, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let catalog = catalog::entries();
        let mut entries = vec![MapEntry {
            id: "downline".into(),
            name: "Downline".into(),
            detail: "Original practice course · Linear".into(),
            mode: MapMode::Surf,
            tier: None,
            source: MapSource::Downline,
        }];
        entries.extend(
            catalog
                .iter()
                .filter(|map| map.source_bsp.is_none() || map.download.is_some())
                .cloned()
                .map(|map| {
                    let bonuses = if map.bonuses > 0 {
                        format!(" · {} bonuses", map.bonuses)
                    } else {
                        String::new()
                    };
                    let source = if let Some(download) = &map.download {
                        let stages = if download.stages > 1 {
                            format!(" · {} stages", download.stages)
                        } else {
                            String::new()
                        };
                        let checkpoints = if download.checkpoints > 0 {
                            format!(" · {} checkpoints", download.checkpoints)
                        } else {
                            String::new()
                        };
                        (
                            MapSource::RankedBsp {
                                map: map.id.clone(),
                                path: None,
                            },
                            format!("{stages}{checkpoints}"),
                        )
                    } else {
                        (
                            MapSource::Catalog {
                                slug: map.slug,
                                geometry: map.geometry,
                                visual: map.visual,
                                bonus_pack: map.bonus_pack,
                            },
                            String::new(),
                        )
                    };
                    MapEntry {
                        id: map.id,
                        name: map.name,
                        detail: format!("{} · {}{}{bonuses}", map.author, map.layout, source.1),
                        mode: map.mode,
                        tier: map.tier,
                        source: source.0,
                    }
                }),
        );

        let mut roots: Vec<_> = roots
            .into_iter()
            .filter_map(|root| root.canonicalize().ok())
            .collect();
        roots.sort_unstable();
        roots.dedup();
        let mut files = Vec::new();
        for root in roots {
            if let Ok(list) = fs::read_dir(root) {
                files.extend(
                    list.flatten()
                        .map(|entry| entry.path())
                        .filter(|path| local_map::map_name(path).is_some() && path.is_file()),
                );
            }
        }
        files.sort_unstable();
        files.dedup();
        let mut names = std::collections::HashSet::new();
        let mut ranked_names = std::collections::HashSet::new();
        for path in files {
            let name = local_map::map_name(&path)
                .expect("filtered local map filename")
                .to_owned();
            let metadata = source_bsp::map_metadata(&path);
            if metadata.tier.is_none() || !local_map::has_timer(&path, &metadata) {
                continue;
            }
            let mode = metadata
                .mode
                .unwrap_or_else(|| local_map::default_mode(&path, &name));
            if let Some(map) = catalog::find(&name)
                .filter(|map| map.source_bsp.is_some() && map.download.is_none())
                && !ranked_names.contains(&map.id)
                && source_bsp::ranked_files_match(&path, map)
            {
                ranked_names.insert(map.id.clone());
                entries.push(MapEntry {
                    id: map.id.clone(),
                    name: map.name.clone(),
                    detail: "Source BSP · Ranked".into(),
                    mode: map.mode,
                    tier: map.tier.or(metadata.tier),
                    source: MapSource::RankedBsp {
                        map: map.id.clone(),
                        path: Some(path.clone()),
                    },
                });
            }
            if !names.insert(name.to_ascii_lowercase()) {
                continue;
            }
            entries.push(MapEntry {
                id: format!("local:{name}"),
                name,
                detail: "Local practice".into(),
                mode,
                tier: metadata.tier,
                source: MapSource::Source(path),
            });
        }
        Self {
            entries,
            directory,
            use_bc: false,
        }
    }

    pub fn catalog_source(&self, name: &str) -> Option<MapSource> {
        self.entries.iter().find_map(|entry| match &entry.source {
            MapSource::Catalog { slug, .. }
                if name.eq_ignore_ascii_case(slug) || name.eq_ignore_ascii_case(&entry.id) =>
            {
                Some(entry.source.clone())
            }
            MapSource::RankedBsp { map, .. } if name.eq_ignore_ascii_case(map) => {
                Some(entry.source.clone())
            }
            _ => None,
        })
    }

    pub fn room_source(&self, name: &str, mode: MapMode, managed: bool) -> Option<MapSource> {
        if name.eq_ignore_ascii_case("downline") && mode == MapMode::Surf {
            return Some(MapSource::Downline);
        }
        if catalog::find(name).is_some_and(|entry| entry.mode == mode)
            && let Some(source) = self.catalog_source(name)
        {
            return Some(source);
        }
        if managed {
            return None;
        }
        self.entries.iter().find_map(|entry| {
            (entry.is_local() && entry.mode == mode && entry.name.eq_ignore_ascii_case(name))
                .then(|| entry.source.clone())
        })
    }
}

pub enum LoadEvent {
    Progress(String),
    Finished(Result<Box<LoadedCourse>, String>),
}

pub struct LoadedCourse {
    pub course: Course,
    pub session: Session,
    pub lesson: Option<&'static tutorial::Lesson>,
    pub starter_guide: bool,
}

pub struct LoadJob(Mutex<mpsc::Receiver<LoadEvent>>);

impl LoadJob {
    fn start(
        directory: PathBuf,
        source: MapSource,
        options: VisualLoadOptions,
        starter_guide: bool,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("surf-map-loader".into())
            .spawn(move || {
                let lesson = match &source {
                    MapSource::Tutorial(id) => tutorial::lesson(id),
                    _ => None,
                };
                let status = |status| {
                    let _ = sender.send(LoadEvent::Progress(status));
                };
                let result = source.load_with_visual_options(&directory, options, status);
                let result = result.map(|course| {
                    let _ = sender.send(LoadEvent::Progress("Indexing course collision…".into()));
                    let session = Session::new(&course);
                    Box::new(LoadedCourse {
                        starter_guide: starter_guide && course.name == STARTER_MAP,
                        course,
                        session,
                        lesson,
                    })
                });
                let _ = sender.send(LoadEvent::Finished(result));
            })
            .map_err(|error| format!("Could not start the map loader: {error}"))?;
        Ok(Self(Mutex::new(receiver)))
    }

    pub fn poll(&self) -> Option<LoadEvent> {
        match self.0.lock().ok()?.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(LoadEvent::Finished(Err(
                "The map loader stopped before the course was ready.".into(),
            ))),
        }
    }
}

fn load_catalog(
    root: &Path,
    slug: &str,
    geometry: &str,
    visual: Option<&str>,
    bonus_pack: Option<&str>,
    options: VisualLoadOptions,
    status: impl Fn(String),
) -> Result<Course, String> {
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("Invalid catalog map identifier".into());
    }
    let directory = cache_directory(root, slug);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create the map cache: {error}"))?;
    let agent = agent();
    status("Loading collision geometry…".into());
    let geometry_path = fetch_asset(&agent, &directory, geometry)?;
    let revision =
        catalog::revision(geometry).ok_or("Map catalog is missing a geometry revision")?;
    let manifest = if let Some(url) = visual {
        status("Loading the original visual manifest…".into());
        Some(fetch_asset(&agent, &directory, url)?)
    } else {
        None
    };
    let prepare = |use_bc| {
        if let Some(manifest) = &manifest {
            fetch_visual_packs(&agent, &directory, manifest, use_bc, &status)?;
        }
        status("Preparing the course and original textures…".into());
        surfd_map::load_verified_with_visual_options(
            &geometry_path,
            manifest.as_deref(),
            revision,
            VisualLoadOptions { use_bc, ..options },
        )
    };
    let mut course = if options.use_bc && manifest.is_some() {
        prepare(true).or_else(|error| {
            warn!("Compressed textures for {slug}: {error}");
            status("Loading decoded textures for this map…".into());
            prepare(false)
        })?
    } else {
        prepare(false)?
    };
    course.catalog_revision = Some(revision.to_owned());
    if let Some(url) = bonus_pack {
        match fetch_asset(&agent, &directory, url).and_then(|path| {
            let revision =
                catalog::revision(url).ok_or("Map catalog is missing a bonus revision")?;
            surfd_map::add_bonus_spawns_verified(&mut course, &path, revision)
        }) {
            Ok(()) => {}
            Err(error) => warn!("Bonus practice points for {slug}: {error}"),
        }
    }
    fs::write(directory.join("ready"), b"").ok();
    Ok(course)
}

fn fetch_visual_packs(
    agent: &ureq::Agent,
    directory: &Path,
    manifest: &Path,
    use_bc: bool,
    status: &impl Fn(String),
) -> Result<(), String> {
    let assets = surfd_map::visual_dependencies_for_gpu(manifest, use_bc)?;
    if !assets.is_empty() {
        status(format!("Loading {} original visual packs…", assets.len()));
    }
    for batch in assets.chunks(4) {
        std::thread::scope(|scope| {
            let downloads: Vec<_> = batch
                .iter()
                .map(|url| scope.spawn(move || fetch_asset(agent, directory, url)))
                .collect();
            for download in downloads {
                download
                    .join()
                    .map_err(|_| "Map download worker stopped unexpectedly")??;
            }
            Ok::<_, String>(())
        })?;
    }
    Ok(())
}
