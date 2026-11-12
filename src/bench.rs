use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use bevy::{
    diagnostic::{DiagnosticPath, DiagnosticsStore},
    prelude::*,
    render::gpu_readback::{ReadbackComplete, ReadbackOnce},
};
use bevy_surf::{
    course::Course,
    network::{PlayerInfo, PlayerState, RoomInfo},
};
use serde::Serialize;

use crate::{
    library::MapSource,
    menu::{MenuState, Page},
    online::Online,
    view::{Game, StartupReplay, ViewCamera},
    world::{PvsCounters, RenderLods},
};

const WARMUP: Duration = Duration::from_secs(3);
const SAMPLE: Duration = Duration::from_secs(12);
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(8);
pub const SIZE: UVec2 = UVec2::new(1280, 800);
const OPAQUE_GPU_MS: DiagnosticPath =
    DiagnosticPath::const_new("render/main_opaque_pass_3d/elapsed_gpu");
const OPAQUE_FRAGMENTS: DiagnosticPath =
    DiagnosticPath::const_new("render/main_opaque_pass_3d/fragment_shader_invocations");

pub struct BenchRequest {
    pub source: MapSource,
    pub replay: StartupReplay,
}

#[derive(Resource)]
pub struct FrameBenchmark {
    pub request: Option<BenchRequest>,
    pub replay: Option<StartupReplay>,
    pub start_tick: Option<usize>,
    pub hold_tick: bool,
    pub target: Option<Handle<Image>>,
    load_started: Option<Instant>,
    assets: Option<AssetStats>,
    phase: Phase,
    previous_frame: Option<Instant>,
    max_frame_ms: f64,
    frame_times_ms: Vec<f64>,
    opaque_pass: Option<OpaquePassSamples>,
    pvs_start: Option<PvsCounters>,
    crowd: Option<BenchmarkCrowd>,
    screenshot: Option<PathBuf>,
    report: Option<Report>,
}

struct BenchmarkCrowd {
    count: u8,
    placed: bool,
}

#[derive(Clone, Copy)]
enum Phase {
    Waiting,
    Warmup(Instant),
    Sampling { started: Instant, start_tick: usize },
    Screenshot(Instant),
    Finished,
}

#[derive(Serialize)]
struct AssetStats {
    map: String,
    load_ms: Option<f64>,
    texture_count: usize,
    compressed_texture_count: usize,
    texture_payload_mib: f64,
}

#[derive(Serialize)]
struct Report {
    #[serde(flatten)]
    assets: AssetStats,
    width: u32,
    height: u32,
    frames: usize,
    warmup_seconds: f64,
    sample_seconds: f64,
    average_fps: f64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_frame_ms: f64,
    sample_max_frame_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar_count: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    opaque_pass: Option<OpaquePassReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pvs: Option<PvsReport>,
    start_tick: usize,
    end_tick: usize,
    held_frame: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    seek_tick: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    screenshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    screenshot_error: Option<String>,
}

#[derive(Default)]
struct OpaquePassSamples {
    gpu_ms: DiagnosticSamples,
    fragments: DiagnosticSamples,
}

#[derive(Default)]
struct DiagnosticSamples {
    last_time: Option<Instant>,
    values: Vec<f64>,
}

#[derive(Serialize)]
struct OpaquePassReport {
    gpu_ms: Option<SampleDistribution>,
    fragment_invocations: Option<SampleDistribution>,
}

#[derive(Serialize)]
struct PvsReport {
    transitions: usize,
    target_checks: usize,
    visibility_changes: usize,
    update_ms: f64,
}

impl PvsReport {
    fn new(start: PvsCounters, end: PvsCounters) -> Self {
        Self {
            transitions: end.transitions.saturating_sub(start.transitions),
            target_checks: end.target_checks.saturating_sub(start.target_checks),
            visibility_changes: end
                .visibility_changes
                .saturating_sub(start.visibility_changes),
            update_ms: millis(end.elapsed.saturating_sub(start.elapsed)),
        }
    }
}

#[derive(Serialize)]
struct SampleDistribution {
    count: usize,
    p50: f64,
    p95: f64,
}

impl DiagnosticSamples {
    fn record(&mut self, store: &DiagnosticsStore, path: &DiagnosticPath) {
        let Some(diagnostic) = store.get(path) else {
            return;
        };
        let Some(latest) = diagnostic.measurement() else {
            return;
        };
        if self.last_time == Some(latest.time) {
            return;
        }
        self.last_time = Some(latest.time);
        // Bevy records the world and 3D sky cameras under the same pass name.
        // Their samples share the timestamp assigned by the diagnostic sync.
        let total = diagnostic
            .measurements()
            .filter(|sample| sample.time == latest.time)
            .map(|sample| sample.value)
            .sum::<f64>();
        if total.is_finite() {
            self.values.push(total);
        }
    }

    fn finish(mut self) -> Option<SampleDistribution> {
        if self.values.is_empty() {
            return None;
        }
        self.values.sort_by(f64::total_cmp);
        Some(SampleDistribution {
            count: self.values.len(),
            p50: percentile(&self.values, 0.50),
            p95: percentile(&self.values, 0.95),
        })
    }
}

impl OpaquePassSamples {
    fn record(&mut self, store: &DiagnosticsStore) {
        self.gpu_ms.record(store, &OPAQUE_GPU_MS);
        self.fragments.record(store, &OPAQUE_FRAGMENTS);
    }

    fn finish(self) -> OpaquePassReport {
        OpaquePassReport {
            gpu_ms: self.gpu_ms.finish(),
            fragment_invocations: self.fragments.finish(),
        }
    }
}

impl FrameBenchmark {
    pub fn new(request: BenchRequest) -> Self {
        Self {
            request: Some(request),
            replay: None,
            start_tick: std::env::var("SURF_BENCH_START_TICK")
                .ok()
                .and_then(|value| value.parse().ok()),
            hold_tick: std::env::var_os("SURF_BENCH_HOLD_TICK").is_some(),
            target: None,
            load_started: None,
            assets: None,
            phase: Phase::Waiting,
            previous_frame: None,
            max_frame_ms: 0.0,
            frame_times_ms: Vec::with_capacity(4096),
            opaque_pass: std::env::var("SURF_BENCH_RENDER_DIAGNOSTICS")
                .is_ok_and(|value| value == "1")
                .then(OpaquePassSamples::default),
            pvs_start: None,
            crowd: std::env::var("SURF_BENCH_AVATAR_COUNT")
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|count| (1..=63).contains(count))
                .map(|count| BenchmarkCrowd {
                    count,
                    placed: false,
                }),
            screenshot: std::env::var_os("SURF_BENCH_SCREENSHOT").map(PathBuf::from),
            report: None,
        }
    }

    pub fn render_diagnostics_enabled(&self) -> bool {
        self.opaque_pass.is_some()
    }

    pub fn crowd_enabled(&self) -> bool {
        self.crowd.is_some()
    }

    pub fn started_load(&mut self) {
        self.load_started = Some(Instant::now());
    }

    pub fn record_map(&mut self, course: &Course) {
        let load_ms = self
            .load_started
            .take()
            .map(|start| millis(start.elapsed()));
        let mut compressed = 0;
        let mut bytes = 0_u64;
        for texture in &course.textures {
            bytes += match &texture.bc {
                Some(bc) => {
                    compressed += 1;
                    bc.blocks().len() as u64
                }
                None => texture.rgba.len() as u64,
            };
        }
        self.assets = Some(AssetStats {
            map: course.name.clone(),
            load_ms,
            texture_count: course.textures.len(),
            compressed_texture_count: compressed,
            texture_payload_mib: bytes as f64 / (1024.0 * 1024.0),
        });
    }

    fn finish(
        &mut self,
        screenshot: Result<Option<String>, String>,
        exit: &mut MessageWriter<AppExit>,
    ) {
        let Some(mut report) = self.report.take() else {
            return;
        };
        self.phase = Phase::Finished;
        let status = match screenshot {
            Ok(path) => {
                report.screenshot = path;
                AppExit::Success
            }
            Err(error) => {
                report.screenshot_error = Some(error);
                AppExit::error()
            }
        };
        println!(
            "{}",
            serde_json::to_string(&report).expect("serialize frame benchmark")
        );
        exit.write(status);
    }
}

pub fn place_crowd(
    mut benchmark: ResMut<FrameBenchmark>,
    game: Res<Game>,
    camera: Single<&Transform, With<ViewCamera>>,
    mut online: ResMut<Online>,
) {
    if game.pending.is_some() || !game.watching_replay() {
        return;
    }
    let Some(crowd) = &mut benchmark.crowd else {
        return;
    };
    if crowd.placed {
        return;
    }
    crowd.placed = true;
    online.room = Some(RoomInfo {
        name: "Benchmark crowd".into(),
        map: game.course.name.clone(),
        mode: game.course.map_mode(),
        players: usize::from(crowd.count),
        capacity: 64,
        room_id: None,
        map_remaining_ms: None,
    });
    let forward = *camera.forward();
    let right = *camera.right();
    online.players = (0..crowd.count)
        .map(|index| {
            let distance = 90.0 + f32::from(index / 9) * 35.0;
            let offset = (f32::from(index % 9) - 4.0) * 24.0;
            let position =
                camera.translation + forward * distance + right * offset - Vec3::Y * 36.0;
            PlayerInfo {
                id: u64::from(index) + 1,
                name: format!("Surfer {}", index + 1),
                state: Some(PlayerState {
                    position: position.to_array(),
                    yaw: 0.0,
                    pitch: 0.0,
                    elapsed: 0.0,
                    finished: false,
                    crouched: index % 2 == 0,
                    eye_height: None,
                    motion: None,
                }),
                spectating: None,
                identity: None,
            }
        })
        .collect();
}

pub fn sample(
    mut benchmark: ResMut<FrameBenchmark>,
    game: Res<Game>,
    menu: Res<MenuState>,
    render_lods: Res<RenderLods>,
    diagnostics: Option<Res<DiagnosticsStore>>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    let now = Instant::now();
    match benchmark.phase {
        Phase::Finished => return,
        Phase::Screenshot(started) => {
            if now.duration_since(started) >= SCREENSHOT_TIMEOUT {
                benchmark.finish(Err("Screenshot capture timed out".into()), &mut exit);
            }
            return;
        }
        _ => {}
    }

    let progress = game.replay_progress().filter(|_| {
        benchmark.assets.is_some() && menu.page == Page::Playing && game.pending.is_none()
    });
    let Some((tick, _, paused)) = progress else {
        if !matches!(benchmark.phase, Phase::Waiting) || benchmark.assets.is_some() {
            eprintln!("Benchmark lost its active replay");
            benchmark.phase = Phase::Finished;
            exit.write(AppExit::error());
        }
        return;
    };
    if paused && !benchmark.hold_tick {
        eprintln!("Benchmark replay ended or paused before sampling finished");
        benchmark.phase = Phase::Finished;
        exit.write(AppExit::error());
        return;
    }

    let frame_ms = benchmark
        .previous_frame
        .replace(now)
        .map_or(0.0, |previous| millis(now.duration_since(previous)));
    benchmark.max_frame_ms = benchmark.max_frame_ms.max(frame_ms);
    match benchmark.phase {
        Phase::Waiting => {
            info!(tick, "Desktop benchmark warming up");
            benchmark.phase = Phase::Warmup(now);
        }
        Phase::Warmup(started) if now.duration_since(started) >= WARMUP => {
            info!(tick, "Desktop benchmark sampling frame intervals");
            benchmark.pvs_start = render_lods.pvs_counters();
            benchmark.phase = Phase::Sampling {
                started: now,
                start_tick: tick,
            };
        }
        Phase::Sampling {
            started,
            start_tick,
        } => {
            if let (Some(samples), Some(diagnostics)) = (&mut benchmark.opaque_pass, &diagnostics) {
                samples.record(diagnostics);
            }
            benchmark.frame_times_ms.push(frame_ms);
            let elapsed = now.duration_since(started);
            if elapsed < SAMPLE {
                return;
            }
            if tick <= start_tick && !benchmark.hold_tick {
                eprintln!("Benchmark replay did not advance during sampling");
                benchmark.phase = Phase::Finished;
                exit.write(AppExit::error());
                return;
            }

            benchmark.frame_times_ms.sort_by(f64::total_cmp);
            let times = &benchmark.frame_times_ms;
            let (p50_ms, p95_ms, p99_ms) = (
                percentile(times, 0.50),
                percentile(times, 0.95),
                percentile(times, 0.99),
            );
            let frames = times.len();
            let sample_max_frame_ms = times[frames - 1];
            let sample_seconds = elapsed.as_secs_f64();
            let opaque_pass = benchmark.opaque_pass.take().map(OpaquePassSamples::finish);
            let pvs = benchmark
                .pvs_start
                .zip(render_lods.pvs_counters())
                .map(|(start, end)| PvsReport::new(start, end));
            benchmark.report = Some(Report {
                assets: benchmark.assets.take().expect("benchmark map was recorded"),
                width: SIZE.x,
                height: SIZE.y,
                frames,
                warmup_seconds: WARMUP.as_secs_f64(),
                sample_seconds,
                average_fps: frames as f64 / sample_seconds,
                p50_ms,
                p95_ms,
                p99_ms,
                max_frame_ms: benchmark.max_frame_ms,
                sample_max_frame_ms,
                avatar_count: benchmark.crowd.as_ref().map(|crowd| crowd.count),
                opaque_pass,
                pvs,
                start_tick,
                end_tick: tick,
                held_frame: benchmark.hold_tick,
                seek_tick: benchmark.start_tick,
                screenshot: None,
                screenshot_error: None,
            });
            if let Some(path) = benchmark.screenshot.take() {
                let target = benchmark.target.clone().expect("benchmark render target");
                benchmark.phase = Phase::Screenshot(now);
                commands.spawn(ReadbackOnce::texture(target)).observe(
                    move |capture: On<ReadbackComplete>,
                          mut benchmark: ResMut<FrameBenchmark>,
                          mut exit: MessageWriter<AppExit>| {
                        let result =
                            image::RgbaImage::from_raw(SIZE.x, SIZE.y, capture.data.clone())
                                .map(image::DynamicImage::ImageRgba8)
                                .ok_or_else(|| "Unexpected screenshot size".to_owned())
                                .and_then(|image| {
                                    image.to_rgb8().save(&path).map_err(|error| {
                                        format!("Could not save {} ({error})", path.display())
                                    })
                                })
                                .map(|()| Some(path.to_string_lossy().into_owned()));
                        benchmark.finish(result, &mut exit);
                    },
                );
            } else {
                benchmark.finish(Ok(None), &mut exit);
            }
        }
        _ => {}
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let rank = fraction * (sorted.len() - 1) as f64;
    let before = sorted[rank.floor() as usize];
    let after = sorted[rank.ceil() as usize];
    before + (after - before) * rank.fract()
}
