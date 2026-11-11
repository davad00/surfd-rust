use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};

use bevy::prelude::*;
use bevy_surf::catalog::{
    self,
    files::{agent, cache_directory, fetch_asset},
};
use bevy_surf::{
    course::Course,
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
            Self::Source(path) => source_bsp::load(&path),
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
    pub source: MapSource,
}

impl MapEntry {
    pub fn preview(&self) -> Option<PathBuf> {
        let MapSource::Catalog { slug, .. } = &self.source else {
            return None;
        };
        reference_preview(slug)
    }

    pub fn status(&self, directory: &Path) -> &'static str {
        match &self.source {
            MapSource::Downline | MapSource::Tutorial(_) => "BUILT IN",
            MapSource::Source(_) => "LOCAL BSP",
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
    pub catalog_count: usize,
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
        let directory = environment_directory("SURF_DATA").unwrap_or_else(default_data_directory);
        let catalog = catalog::entries();
        let catalog_count = catalog.len();
        let mut entries = vec![MapEntry {
            id: "downline".into(),
            name: "Downline".into(),
            detail: "Original practice course · Linear".into(),
            source: MapSource::Downline,
        }];
        entries.extend(catalog.iter().cloned().map(|map| {
            let tier = map
                .tier
                .map_or(String::new(), |tier| format!(" · Tier {tier}"));
            let bonuses = if map.bonuses > 0 {
                format!(" · {} bonuses", map.bonuses)
            } else {
                String::new()
            };
            MapEntry {
                id: map.id,
                name: map.name,
                detail: format!("{} · {}{tier}{bonuses}", map.author, map.layout),
                source: MapSource::Catalog {
                    slug: map.slug,
                    geometry: map.geometry,
                    visual: map.visual,
                    bonus_pack: map.bonus_pack,
                },
            }
        }));

        let mut roots = vec![directory.clone()];
        if let Some(configured) = std::env::var_os("SURF_MAPS") {
            roots.extend(std::env::split_paths(&configured));
        }
        let mut files = Vec::new();
        for root in roots {
            if let Ok(list) = fs::read_dir(root) {
                files.extend(list.flatten().map(|entry| entry.path()).filter(|path| {
                    path.extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("bsp"))
                        && path.is_file()
                }));
            }
        }
        files.sort_unstable();
        files.dedup();
        for path in files {
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            entries.push(MapEntry {
                id: name.clone(),
                name,
                detail: "Counter-Strike: Source · Original BSP".into(),
                source: MapSource::Source(path),
            });
        }
        Self {
            entries,
            directory,
            catalog_count,
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
            _ => None,
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
