use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::window::PrimaryWindow;
use image::{ImageFormat, ImageReader, Limits, RgbaImage};
use serde::Deserialize;

use crate::library::{MapLibrary, reference_preview};
use crate::menu::{self, MenuState, Page};

const RELEASE: &str = "https://surfd-assets.classic-surf.workers.dev/releases/8009868addd09442f91d4d491a4046c952b6cebd";
const MANIFEST: &str = "/menu/flythrough/manifest.json";
const POSTER_PREFIX: &str = "/menu/flythrough/";
const MANIFEST_BYTES: usize = 64 * 1024;
const POSTER_BYTES: usize = 4 * 1024 * 1024;
const CLIP_BYTES: usize = 12 * 1024 * 1024;
const POSTER_SECONDS: f32 = 11.0;
const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;
const FRAME_BYTES: usize = WIDTH as usize * HEIGHT as usize * 4;
const FRAME_SECONDS: f32 = 1.0 / 30.0;

pub struct FlythroughPlugin;

impl Plugin for FlythroughPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Flythrough>();
        app.add_systems(Update, update_backdrop.after(menu::render));
    }
}

#[derive(Component)]
pub struct BackdropImage;

#[derive(Component)]
pub struct BackdropCaption;

#[derive(Deserialize)]
struct Clip {
    slug: String,
    title: String,
    author: String,
    poster: String,
    src: String,
}

impl Clip {
    fn caption(&self) -> String {
        format!("{}  ·  {}", self.title, self.author)
    }
}

enum Load {
    Catalogue(Vec<Clip>),
    Poster(RgbaImage, String),
}

#[derive(Default, Resource)]
struct Flythrough {
    started: bool,
    catalogue: Vec<Clip>,
    current: Option<usize>,
    job: Option<Mutex<mpsc::Receiver<Load>>>,
    elapsed: f32,
    poster: Option<(Handle<Image>, Vec2, String)>,
    playback: Option<Playback>,
    video_attempted: bool,
    video: Option<Handle<Image>>,
}

fn update_backdrop(
    mut state: ResMut<Flythrough>,
    menu: Res<MenuState>,
    library: Res<MapLibrary>,
    time: Res<Time<Real>>,
    mut images: ResMut<Assets<Image>>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut nodes: Query<(&mut ImageNode, &mut Node, &mut Visibility), With<BackdropImage>>,
    mut captions: Query<&mut Text, With<BackdropCaption>>,
) {
    if menu.page != Page::Home {
        state.playback = None;
        state.video_attempted = false;
        state.video = None;
        state.elapsed = 0.0;
        return;
    }
    let directory = library.directory.join("flythrough-8009868a");
    if !state.started {
        state.started = true;
        let dir = directory.clone();
        state.job = start_job(move |sender| {
            if let Some(image) = local_poster("boreas") {
                let _ = sender.send(Load::Poster(image, "Boreas  ·  Syncronyze".into()));
            }
            if let Some(catalogue) = load_catalogue(&dir) {
                let _ = sender.send(Load::Catalogue(catalogue));
            }
        });
    }
    state.elapsed += time.delta_secs();
    loop {
        let Some(job) = &state.job else {
            break;
        };
        let event = job.lock().expect("flythrough receiver poisoned").try_recv();
        match event {
            Ok(Load::Catalogue(catalogue)) => state.catalogue = catalogue,
            Ok(Load::Poster(pixels, caption)) => {
                let size = Vec2::new(pixels.width() as f32, pixels.height() as f32);
                let image = images.add(Image::new(
                    Extent3d {
                        width: pixels.width(),
                        height: pixels.height(),
                        depth_or_array_layers: 1,
                    },
                    TextureDimension::D2,
                    pixels.into_raw(),
                    TextureFormat::Rgba8UnormSrgb,
                    RenderAssetUsages::default(),
                ));
                state.poster = Some((image, size, caption));
                state.elapsed = 0.0;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                state.job = None;
                break;
            }
        }
    }
    if state.job.is_none()
        && !state.catalogue.is_empty()
        && (state.current.is_none() || state.elapsed >= POSTER_SECONDS)
    {
        let index = state
            .current
            .map_or(0, |index| (index + 1) % state.catalogue.len());
        let clip = &state.catalogue[index];
        let (poster, slug, caption) = (clip.poster.clone(), clip.slug.clone(), clip.caption());
        state.current = Some(index);
        state.elapsed = 0.0;
        state.playback = None;
        state.video_attempted = false;
        state.video = None;
        let directory = directory.clone();
        state.job = start_job(move |sender| {
            if let Some(image) = load_poster(&directory, &poster).or_else(|| local_poster(&slug)) {
                let _ = sender.send(Load::Poster(image, caption));
            }
        });
    }
    if !menu.settings.menu_motion {
        state.playback = None;
        state.video = None;
    } else if let Some(index) = state.current
        && !state.video_attempted
    {
        state.video_attempted = true;
        state.playback = Playback::start(directory, state.catalogue[index].src.clone());
    }
    if let Some(frame) = state
        .playback
        .as_mut()
        .and_then(|playback| playback.next_frame(time.delta_secs()))
    {
        if let Some(mut image) = state
            .video
            .as_ref()
            .and_then(|handle| images.get_mut(handle))
        {
            image.data = Some(frame);
        } else {
            state.video = Some(images.add(Image::new(
                Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                frame,
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            )));
        }
    }
    let video_caption = state
        .video
        .as_ref()
        .and_then(|_| state.current)
        .and_then(|index| state.catalogue.get(index))
        .map(Clip::caption);
    let (image, size, caption) =
        if let (Some(video), Some(caption)) = (&state.video, &video_caption) {
            (video, Vec2::new(WIDTH as f32, HEIGHT as f32), caption)
        } else if let Some((poster, size, caption)) = &state.poster {
            (poster, *size, caption)
        } else {
            return;
        };
    let viewport = Vec2::new(window.width(), window.height());
    let factor = (viewport.x / size.x).max(viewport.y / size.y);
    let covered = size * factor;
    let offset = (viewport - covered) / 2.0;
    for (mut node_image, mut layout, mut visibility) in &mut nodes {
        if node_image.image != *image {
            node_image.image = image.clone();
        }
        if layout.width != px(covered.x)
            || layout.height != px(covered.y)
            || layout.left != px(offset.x)
            || layout.top != px(offset.y)
        {
            layout.width = px(covered.x);
            layout.height = px(covered.y);
            layout.left = px(offset.x);
            layout.top = px(offset.y);
        }
        if *visibility != Visibility::Inherited {
            *visibility = Visibility::Inherited;
        }
    }
    for mut label in &mut captions {
        if label.0 != *caption {
            label.0.clone_from(caption);
        }
    }
}

fn start_job(
    work: impl FnOnce(mpsc::Sender<Load>) + Send + 'static,
) -> Option<Mutex<mpsc::Receiver<Load>>> {
    let (send, receive) = mpsc::channel();
    std::thread::Builder::new()
        .name("surf-flythrough".into())
        .spawn(move || work(send))
        .ok()?;
    Some(Mutex::new(receive))
}

fn load_catalogue(directory: &Path) -> Option<Vec<Clip>> {
    let path = directory.join("manifest.json");
    if let Some(clips) =
        read_bytes(File::open(&path).ok(), MANIFEST_BYTES).and_then(parse_catalogue)
    {
        return Some(clips);
    }
    let bytes = download(MANIFEST, MANIFEST_BYTES)?;
    let clips = parse_catalogue(bytes.clone())?;
    cache(&path, &bytes);
    Some(clips)
}

fn parse_catalogue(bytes: Vec<u8>) -> Option<Vec<Clip>> {
    let mut clips: Vec<Clip> = serde_json::from_slice(&bytes).ok()?;
    clips.retain(|clip| {
        safe_slug(&clip.slug)
            && !clip.title.is_empty()
            && clip.title.len() <= 80
            && !clip.author.is_empty()
            && clip.author.len() <= 100
            && poster_filename(&clip.poster).is_some()
            && clip_filename(&clip.src).is_some()
    });
    clips.truncate(20);
    if let Some(boreas) = clips.iter().position(|clip| clip.slug == "boreas") {
        clips.rotate_left(boreas);
    }
    (!clips.is_empty()).then_some(clips)
}

fn safe_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 60
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn poster_filename(path: &str) -> Option<&str> {
    let name = path.strip_prefix(POSTER_PREFIX)?;
    safe_slug(name.strip_suffix(".jpg")?).then_some(name)
}

fn clip_filename(path: &str) -> Option<&str> {
    let name = path.strip_prefix(POSTER_PREFIX)?;
    safe_slug(name.strip_suffix(".webm")?).then_some(name)
}

fn load_poster(directory: &Path, route: &str) -> Option<RgbaImage> {
    let path = directory.join(poster_filename(route)?);
    if let Some(poster) =
        read_bytes(File::open(&path).ok(), POSTER_BYTES).and_then(|bytes| decode(&bytes))
    {
        return Some(poster);
    }
    let bytes = download(route, POSTER_BYTES)?;
    let poster = decode(&bytes)?;
    cache(&path, &bytes);
    Some(poster)
}

fn local_poster(slug: &str) -> Option<RgbaImage> {
    if !safe_slug(slug) {
        return None;
    }
    let path = reference_preview(slug)?;
    let bytes = read_bytes(File::open(path).ok(), POSTER_BYTES)?;
    decode(&bytes)
}

fn download(route: &str, cap: usize) -> Option<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(12)))
        .build()
        .into();
    let mut response = agent.get(format!("{RELEASE}{route}")).call().ok()?;
    read_bytes(Some(response.body_mut().as_reader()), cap)
}

fn read_bytes(reader: Option<impl Read>, cap: usize) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    reader?.take(cap as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= cap).then_some(bytes)
}

fn decode(bytes: &[u8]) -> Option<RgbaImage> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Jpeg);
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    Some(reader.decode().ok()?.into_rgba8())
}

fn cache(path: &Path, bytes: &[u8]) {
    if let Some(directory) = path.parent()
        && fs::create_dir_all(directory).is_ok()
    {
        let _ = fs::write(path, bytes);
    }
}

struct Playback {
    cancelled: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    frames: Mutex<mpsc::Receiver<Vec<u8>>>,
    clock: f32,
}

impl Playback {
    fn start(directory: PathBuf, route: String) -> Option<Self> {
        let program = ffmpeg_program()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let (send, receive) = mpsc::sync_channel(2);
        let worker_cancelled = cancelled.clone();
        let worker_child = child.clone();
        std::thread::Builder::new()
            .name("surf-flythrough-video".into())
            .spawn(move || {
                let Some(path) = load_clip(&directory, &route, &worker_cancelled) else {
                    return;
                };
                decode_clip(&program, &path, &worker_cancelled, &worker_child, send);
            })
            .ok()?;
        Some(Self {
            cancelled,
            child,
            frames: Mutex::new(receive),
            clock: 0.0,
        })
    }

    fn next_frame(&mut self, delta: f32) -> Option<Vec<u8>> {
        self.clock = (self.clock + delta).min(3.0 * FRAME_SECONDS);
        let mut frame = None;
        let frames = self.frames.get_mut().ok()?;
        while self.clock >= FRAME_SECONDS {
            let Ok(next) = frames.try_recv() else {
                break;
            };
            self.clock -= FRAME_SECONDS;
            frame = Some(next);
        }
        frame
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Ok(mut child) = self.child.lock()
            && let Some(child) = child.as_mut()
        {
            let _ = child.kill();
        }
    }
}

fn ffmpeg_program() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SURF_FFMPEG")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Some(path);
    }
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("ChocolateyInstall")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("ProgramData").map(|root| PathBuf::from(root).join("chocolatey"))
        })
    {
        // Launch the decoder itself so cancellation does not just terminate a package-manager shim.
        let executable = root.join("lib/ffmpeg/tools/ffmpeg/bin/ffmpeg.exe");
        if executable.is_file() {
            return Some(executable);
        }
    }
    let filename = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|path| path.join(filename))
        .find(|path| path.is_file())
}

fn load_clip(directory: &Path, route: &str, cancelled: &AtomicBool) -> Option<PathBuf> {
    const EBML: [u8; 4] = [0x1a, 0x45, 0xdf, 0xa3];
    let path = directory.join(clip_filename(route)?);
    if let Ok(mut file) = File::open(&path)
        && file.metadata().ok()?.len() <= CLIP_BYTES as u64
    {
        let mut signature = [0; 4];
        if file.read_exact(&mut signature).is_ok() && signature == EBML {
            return Some(path);
        }
    }
    if cancelled.load(Ordering::Acquire) {
        return None;
    }
    let bytes = download(route, CLIP_BYTES)?;
    if cancelled.load(Ordering::Acquire) || !bytes.starts_with(&EBML) {
        return None;
    }
    cache(&path, &bytes);
    path.is_file().then_some(path)
}

fn decode_clip(
    program: &Path,
    path: &Path,
    cancelled: &AtomicBool,
    child_slot: &Mutex<Option<Child>>,
    send: mpsc::SyncSender<Vec<u8>>,
) {
    if cancelled.load(Ordering::Acquire) {
        return;
    }
    let mut command = Command::new(program);
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-an",
            "-vf",
            "scale=960:540:flags=bilinear,fps=30",
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let Ok(mut child) = command.spawn() else {
        return;
    };
    let Some(mut output) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    {
        let mut slot = child_slot.lock().expect("flythrough child lock poisoned");
        if cancelled.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        *slot = Some(child);
    }
    while !cancelled.load(Ordering::Acquire) {
        let mut frame = vec![0; FRAME_BYTES];
        if output.read_exact(&mut frame).is_err() || send.send(frame).is_err() {
            break;
        }
    }
    let child = child_slot.lock().ok().and_then(|mut slot| slot.take());
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
}
