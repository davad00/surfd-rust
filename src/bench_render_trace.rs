use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use bevy::{
    app::SubApp,
    core_pipeline::core_3d::prepare_core_3d_depth_textures,
    ecs::schedule::IntoSystemSet,
    pbr::{
        MeshPipeline, collect_meshes_for_gpu_building, prepare_mesh_bind_groups,
        prepare_mesh_view_bind_groups, prepare_preprocess_bind_groups,
        write_mesh_culling_data_buffer,
    },
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        batching::gpu_preprocessing::write_batched_instance_buffers,
        material_bind_groups::prepare_material_bind_groups,
        mesh::allocator::allocate_and_free_meshes,
        pipelined_rendering::RenderExtractApp,
        render_asset::prepare_assets,
        render_resource::{PipelineCache, PipelineDescriptor},
        renderer::{RenderGraph, RenderGraphSystems, render_system},
        texture::GpuImage,
        view::prepare_view_targets,
    },
};

const SLOW: Duration = Duration::from_millis(50);
const PIPELINE_REPORT_LIMIT: u8 = 12;
const PIPELINE_LABEL_LIMIT: usize = 8;

const PREPARE_SETS: [(RenderSystems, &str); 12] = [
    (RenderSystems::PrepareMeshes, "prepare_meshes"),
    (RenderSystems::CreateViews, "create_views"),
    (RenderSystems::Specialize, "specialize"),
    (RenderSystems::PrepareViews, "prepare_views"),
    (RenderSystems::Queue, "queue"),
    (RenderSystems::PhaseSort, "phase_sort"),
    (RenderSystems::PrepareResources, "prepare_resources"),
    (
        RenderSystems::PrepareResourcesBatchPhases,
        "prepare_batches",
    ),
    (
        RenderSystems::PrepareResourcesWritePhaseBuffers,
        "write_phase_buffers",
    ),
    (
        RenderSystems::PrepareResourcesCollectPhaseBuffers,
        "collect_phase_buffers",
    ),
    (RenderSystems::PrepareResourcesFlush, "flush_resources"),
    (RenderSystems::PrepareBindGroups, "prepare_bind_groups"),
];

const PREPARE_SYSTEM_NAMES: [&str; 9] = [
    "collect_gpu_meshes",
    "view_targets",
    "depth_textures",
    "mesh_culling_upload",
    "mesh_instance_upload",
    "material_bind_groups",
    "mesh_bind_groups",
    "mesh_view_bind_groups",
    "preprocess_bind_groups",
];

const PREPARE_SPAN_COUNT: usize = PREPARE_SETS.len() + PREPARE_SYSTEM_NAMES.len();

pub fn install(app: &mut App) {
    if !std::env::var("SURF_BENCH_RENDER_TRACE").is_ok_and(|value| value == "1") {
        return;
    }
    let origin = Instant::now();
    let Some(render) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    trace_extract(render, origin, "scene extract");
    render
        .insert_resource(RenderTrace::new(origin))
        .insert_resource(PrepareTrace::new(origin))
        .add_systems(
            Render,
            (
                begin_render.before(RenderSystems::ExtractCommands),
                stamp(Mark::AssetsBegin)
                    .after(RenderSystems::ExtractCommands)
                    .before(RenderSystems::PrepareAssets),
                stamp(Mark::ImagesBegin)
                    .in_set(RenderSystems::PrepareAssets)
                    .before(prepare_assets::<GpuImage>),
                stamp(Mark::ImagesEnd)
                    .in_set(RenderSystems::PrepareAssets)
                    .after(prepare_assets::<GpuImage>),
                stamp(Mark::MeshAllocBegin)
                    .in_set(RenderSystems::PrepareAssets)
                    .before(allocate_and_free_meshes),
                stamp(Mark::MeshAllocEnd)
                    .in_set(RenderSystems::PrepareAssets)
                    .after(allocate_and_free_meshes),
                stamp(Mark::AssetsEnd)
                    .after(RenderSystems::PrepareAssets)
                    .before(RenderSystems::PrepareMeshes),
                stamp(Mark::PrepareEnd)
                    .after(RenderSystems::Prepare)
                    .before(RenderSystems::Render),
                stamp(Mark::RenderEnd)
                    .after(render_system)
                    .before(RenderSystems::Cleanup),
                end_render.after(RenderSystems::PostCleanup),
            ),
        )
        .add_systems(
            RenderGraph,
            (
                stamp(Mark::GraphBegin).before(RenderGraphSystems::Begin),
                stamp(Mark::SubmitBegin)
                    .after(RenderGraphSystems::Render)
                    .before(RenderGraphSystems::Submit),
                stamp(Mark::SubmitEnd)
                    .after(RenderGraphSystems::Submit)
                    .in_set(RenderGraphSystems::Finish),
                stamp(Mark::GraphEnd).after(RenderGraphSystems::Finish),
            ),
        );
    trace_prepare(render);
    if let Some(handoff) = app.get_sub_app_mut(RenderExtractApp) {
        trace_extract(handoff, origin, "render handoff");
    }
    app.insert_resource(MainTrace {
        origin,
        frame: 0,
        started: origin,
        previous_end: None,
        slow: SlowReports::default(),
    })
    .add_systems(First, begin_main)
    .add_systems(Last, end_main);
}

fn trace_prepare(render: &mut SubApp) {
    // Each pair only brackets its own set. Preserve Bevy's weak ordering
    // between the sets so unrelated work can still overlap.
    for (index, (set, _)) in PREPARE_SETS.into_iter().enumerate() {
        render.add_systems(
            Render,
            (
                stamp_prepare(index, 0)
                    .after(begin_render)
                    .before(set.clone()),
                stamp_prepare(index, 1)
                    .after(begin_render)
                    .after(set)
                    .before(RenderSystems::Render),
            ),
        );
    }
    trace_prepare_system(
        render,
        0,
        RenderSystems::PrepareMeshes,
        collect_meshes_for_gpu_building,
    );
    trace_prepare_system(render, 1, RenderSystems::PrepareViews, prepare_view_targets);
    trace_prepare_system(
        render,
        2,
        RenderSystems::PrepareResources,
        prepare_core_3d_depth_textures,
    );
    trace_prepare_system(
        render,
        3,
        RenderSystems::PrepareResourcesFlush,
        write_mesh_culling_data_buffer,
    );
    trace_prepare_system(
        render,
        4,
        RenderSystems::PrepareResourcesFlush,
        write_batched_instance_buffers::<MeshPipeline>,
    );
    trace_prepare_system(
        render,
        5,
        RenderSystems::PrepareBindGroups,
        prepare_material_bind_groups,
    );
    trace_prepare_system(
        render,
        6,
        RenderSystems::PrepareBindGroups,
        prepare_mesh_bind_groups,
    );
    trace_prepare_system(
        render,
        7,
        RenderSystems::PrepareBindGroups,
        prepare_mesh_view_bind_groups,
    );
    trace_prepare_system(
        render,
        8,
        RenderSystems::PrepareBindGroups,
        prepare_preprocess_bind_groups,
    );
}

fn trace_prepare_system<M>(
    render: &mut SubApp,
    index: usize,
    set: RenderSystems,
    system: impl IntoSystemSet<M> + Copy,
) {
    let index = PREPARE_SETS.len() + index;
    render.add_systems(
        Render,
        (
            stamp_prepare(index, 0)
                .in_set(set.clone())
                .after(begin_render)
                .before(system),
            stamp_prepare(index, 1)
                .in_set(set)
                .after(begin_render)
                .after(system),
        ),
    );
}

fn stamp_prepare(index: usize, end: usize) -> impl FnMut(Res<PrepareTrace>) {
    move |trace| {
        trace.spans[index][end].store(ms(trace.origin.elapsed()).to_bits(), Ordering::Relaxed);
    }
}

#[derive(Default)]
struct SlowReports {
    count: u8,
    max: Duration,
}

impl SlowReports {
    fn record(&mut self, elapsed: Duration) -> bool {
        let new_max = elapsed > self.max;
        self.max = self.max.max(elapsed);
        if elapsed < SLOW || self.count >= 24 || (self.count >= 12 && !new_max) {
            return false;
        }
        self.count += 1;
        true
    }
}

#[derive(Resource)]
struct MainTrace {
    origin: Instant,
    frame: u64,
    started: Instant,
    previous_end: Option<Instant>,
    slow: SlowReports,
}

fn begin_main(mut trace: ResMut<MainTrace>) {
    trace.frame += 1;
    trace.started = Instant::now();
}

fn end_main(mut trace: ResMut<MainTrace>) {
    let now = Instant::now();
    let between = trace
        .previous_end
        .replace(now)
        .map_or(Duration::ZERO, |end| trace.started.duration_since(end));
    let main = now.duration_since(trace.started);
    if trace.slow.record(main + between) {
        info!(
            main_frame = trace.frame,
            at_ms = ms(trace.started.duration_since(trace.origin)),
            main_ms = ms(main),
            between_main_ms = ms(between),
            "Benchmark main frame"
        );
    }
}

fn trace_extract(app: &mut SubApp, origin: Instant, name: &'static str) {
    let Some(mut extract) = app.take_extract() else {
        return;
    };
    let mut slow = SlowReports::default();
    app.set_extract(move |main, render| {
        let main_frame = main
            .get_resource::<MainTrace>()
            .map_or(0, |trace| trace.frame);
        let started = Instant::now();
        extract(main, render);
        let elapsed = started.elapsed();
        if let Some(mut trace) = render.get_resource_mut::<RenderTrace>() {
            trace.main_frame = main_frame;
        }
        if slow.record(elapsed) {
            info!(
                main_frame,
                stage = name,
                at_ms = ms(started.duration_since(origin)),
                elapsed_ms = ms(elapsed),
                "Benchmark render extract"
            );
        }
    });
}

#[derive(Clone, Copy)]
enum Mark {
    FrameBegin,
    AssetsBegin,
    ImagesBegin,
    ImagesEnd,
    MeshAllocBegin,
    MeshAllocEnd,
    AssetsEnd,
    PrepareEnd,
    GraphBegin,
    SubmitBegin,
    SubmitEnd,
    GraphEnd,
    RenderEnd,
}

const MARK_COUNT: usize = Mark::RenderEnd as usize + 1;

#[derive(Resource)]
struct RenderTrace {
    origin: Instant,
    main_frame: u64,
    marks: [Option<Instant>; MARK_COUNT],
    slow: SlowReports,
    pipeline_reports: u8,
    pipeline_count: usize,
    waiting_count: usize,
}

impl RenderTrace {
    fn new(origin: Instant) -> Self {
        Self {
            origin,
            main_frame: 0,
            marks: [None; MARK_COUNT],
            slow: SlowReports::default(),
            pipeline_reports: 0,
            pipeline_count: 0,
            waiting_count: 0,
        }
    }

    fn span(&self, start: Mark, end: Mark) -> f64 {
        self.marks[start as usize]
            .zip(self.marks[end as usize])
            .map_or(0.0, |(start, end)| ms(end.duration_since(start)))
    }
}

#[derive(Resource)]
struct PrepareTrace {
    origin: Instant,
    // Independent stamps are read-only to Bevy's scheduler, so they don't
    // turn the render schedule's weak edges into resource conflicts.
    spans: [[AtomicU64; 2]; PREPARE_SPAN_COUNT],
}

impl PrepareTrace {
    fn new(origin: Instant) -> Self {
        Self {
            origin,
            spans: Default::default(),
        }
    }

    fn intervals(&self, frame_start: Instant) -> Vec<(&str, f64, f64)> {
        let frame_start_ms = ms(frame_start.duration_since(self.origin));
        PREPARE_SETS
            .iter()
            .map(|(_, label)| *label)
            .chain(PREPARE_SYSTEM_NAMES)
            .zip(&self.spans)
            .filter_map(|(label, [start, end])| {
                let start = f64::from_bits(start.load(Ordering::Relaxed));
                let end = f64::from_bits(end.load(Ordering::Relaxed));
                (start >= frame_start_ms && end >= start).then_some((
                    label,
                    start - frame_start_ms,
                    end - frame_start_ms,
                ))
            })
            .collect()
    }
}

fn begin_render(mut trace: ResMut<RenderTrace>, prepare: Res<PrepareTrace>) {
    trace.marks = [None; MARK_COUNT];
    for mark in prepare.spans.iter().flatten() {
        mark.store(0, Ordering::Relaxed);
    }
    trace.marks[Mark::FrameBegin as usize] = Some(Instant::now());
}

fn stamp(mark: Mark) -> impl FnMut(ResMut<RenderTrace>) {
    move |mut trace| trace.marks[mark as usize] = Some(Instant::now())
}

fn end_render(
    mut trace: ResMut<RenderTrace>,
    prepare: Res<PrepareTrace>,
    cache: Res<PipelineCache>,
) {
    let now = Instant::now();
    let Some(started) = trace.marks[Mark::FrameBegin as usize] else {
        return;
    };
    let elapsed = now.duration_since(started);
    let slow = trace.slow.record(elapsed);
    let count = cache.pipelines().count();
    let waiting = cache.waiting_pipelines().count();
    let previous_count = std::mem::replace(&mut trace.pipeline_count, count);
    let previous_waiting = std::mem::replace(&mut trace.waiting_count, waiting);
    let queue_changed = count != previous_count || waiting != previous_waiting;
    if slow {
        use Mark::*;
        info!(
            main_frame = trace.main_frame,
            at_ms = ms(started.duration_since(trace.origin)),
            render_ms = ms(elapsed),
            extract_commands_ms = trace.span(FrameBegin, AssetsBegin),
            assets_ms = trace.span(AssetsBegin, AssetsEnd),
            image_prepare_ms = trace.span(ImagesBegin, ImagesEnd),
            mesh_allocate_ms = trace.span(MeshAllocBegin, MeshAllocEnd),
            other_prepare_ms = trace.span(AssetsEnd, PrepareEnd),
            pipeline_queue_ms = trace.span(PrepareEnd, GraphBegin),
            graph_encode_ms = trace.span(GraphBegin, SubmitBegin),
            graph_submit_ms = trace.span(SubmitBegin, SubmitEnd),
            graph_finish_ms = trace.span(SubmitEnd, GraphEnd),
            final_submit_present_ms = trace.span(GraphEnd, RenderEnd),
            "Benchmark render frame"
        );
        info!(
            main_frame = trace.main_frame,
            prepare_intervals_ms = ?prepare.intervals(started),
            "Benchmark render preparation"
        );
    }
    if slow || (queue_changed && trace.pipeline_reports < PIPELINE_REPORT_LIMIT) {
        trace.pipeline_reports += u8::from(!slow);
        let new = cache
            .pipelines()
            .skip(previous_count.min(count))
            .take(PIPELINE_LABEL_LIMIT)
            .map(|pipeline| pipeline_label(&pipeline.descriptor))
            .collect::<Vec<_>>();
        let pending = cache
            .waiting_pipelines()
            .take(PIPELINE_LABEL_LIMIT)
            .filter_map(|id| cache.pipelines().nth(id))
            .map(|pipeline| pipeline_label(&pipeline.descriptor))
            .collect::<Vec<_>>();
        info!(
            main_frame = trace.main_frame,
            at_ms = ms(now.duration_since(trace.origin)),
            pipelines = count,
            waiting,
            new = ?new,
            pending = ?pending,
            "Benchmark pipeline queue"
        );
    }
}

fn pipeline_label(descriptor: &PipelineDescriptor) -> String {
    let (kind, label, shader) = match descriptor {
        PipelineDescriptor::RenderPipelineDescriptor(descriptor) => (
            "render",
            descriptor.label.as_deref(),
            &descriptor.vertex.shader,
        ),
        PipelineDescriptor::ComputePipelineDescriptor(descriptor) => {
            ("compute", descriptor.label.as_deref(), &descriptor.shader)
        }
        PipelineDescriptor::MeshPipelineDescriptor(descriptor) => {
            ("mesh", descriptor.label.as_deref(), &descriptor.mesh.shader)
        }
    };
    let label = label.unwrap_or("unnamed");
    shader.path().map_or_else(
        || format!("{kind} {label}"),
        |path| format!("{kind} {label} ({path})"),
    )
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
