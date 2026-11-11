use bevy::{
    app::{PluginGroupBuilder, TaskPoolOptions, TaskPoolPlugin},
    pbr::{
        BuildIndirectParametersPipeline, BuildIndirectParametersPipelineKey, PreprocessPipelines,
    },
    prelude::*,
    render::{
        RenderApp, RenderPlugin, RenderStartup,
        batching::gpu_preprocessing::GpuPreprocessingSupport,
        init_gpu_resource,
        render_resource::{PipelineCache, SpecializedComputePipelines},
        renderer::{RenderAdapterInfo, RenderDevice},
        settings::{Backends, Dx12Compiler, RenderCreation, WgpuFeatures, WgpuSettings},
    },
};

type IndirectBuildPipelines = SpecializedComputePipelines<BuildIndirectParametersPipeline>;

pub(super) fn configure(plugins: PluginGroupBuilder) -> PluginGroupBuilder {
    let plugins = if let Ok(threads) = std::thread::available_parallelism()
        && (4..=5).contains(&threads.get())
    {
        // The default leaves one background worker on these hosts. Give
        // pipeline compilation a second worker and keep the compute budget.
        let mut task_pool_options = TaskPoolOptions::default();
        task_pool_options.async_compute.min_threads = 2;
        task_pool_options.compute.min_threads = threads.get() - 2;
        plugins.set(TaskPoolPlugin { task_pool_options })
    } else {
        plugins
    };
    let settings = WgpuSettings::default();
    let uses_fxc = matches!(settings.dx12_shader_compiler, Dx12Compiler::Fxc);
    plugins
        .set(RenderPlugin {
            render_creation: RenderCreation::Automatic(Box::new(settings)),
            ..default()
        })
        .add_after::<RenderPlugin>(Dx12PipelineCompatibility { uses_fxc })
}

struct Dx12PipelineCompatibility {
    uses_fxc: bool,
}

impl Plugin for Dx12PipelineCompatibility {
    fn build(&self, app: &mut App) {
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(
                RenderStartup,
                prepare_dx12_indirect_pipelines
                    .after(init_gpu_resource::<GpuPreprocessingSupport>)
                    .after(init_gpu_resource::<PreprocessPipelines>)
                    .after(init_gpu_resource::<IndirectBuildPipelines>),
            );
        }
    }

    fn finish(&self, app: &mut App) {
        if !self.uses_fxc {
            return;
        }
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        let world = render_app.world_mut();
        if world
            .get_resource::<RenderAdapterInfo>()
            .is_some_and(|adapter| Backends::from(adapter.backend) == Backends::DX12)
        {
            // RenderPlugin has just created the cache. Serialize FXC pipeline
            // compilation before downstream plugins can queue work.
            let device = world.resource::<RenderDevice>().clone();
            world.insert_resource(PipelineCache::new(device, true));
        }
    }
}

fn prepare_dx12_indirect_pipelines(
    adapter: Res<RenderAdapterInfo>,
    device: Res<RenderDevice>,
    support: Res<GpuPreprocessingSupport>,
    cache: Res<PipelineCache>,
    builds: Option<ResMut<IndirectBuildPipelines>>,
    preprocess: Option<ResMut<PreprocessPipelines>>,
) {
    if Backends::from(adapter.backend) != Backends::DX12
        || !device
            .features()
            .contains(WgpuFeatures::MULTI_DRAW_INDIRECT_COUNT)
    {
        return;
    }
    let (Some(mut builds), Some(preprocess)) = (builds, preprocess) else {
        return;
    };
    let preprocess = preprocess.into_inner();

    // Bevy 0.20 draws fixed indirect slots on DX12. Prepare the matching
    // producer variants before Bevy can pick count-based compaction.
    use BuildIndirectParametersPipelineKey as Key;
    let mut prepare_pair = |indexed: &mut BuildIndirectParametersPipeline,
                            non_indexed: &mut BuildIndirectParametersPipeline,
                            key: Key| {
        indexed.pipeline_id = Some(builds.specialize(&cache, indexed, key | Key::INDEXED));
        non_indexed.pipeline_id = Some(builds.specialize(&cache, non_indexed, key));
    };
    prepare_pair(
        &mut preprocess.gpu_frustum_culling_build_indexed_indirect_params,
        &mut preprocess.gpu_frustum_culling_build_non_indexed_indirect_params,
        Key::empty(),
    );
    if support.is_culling_supported() {
        for (phase, key) in [
            (&mut preprocess.early_phase, Key::EARLY_PHASE),
            (&mut preprocess.late_phase, Key::LATE_PHASE),
            (&mut preprocess.main_phase, Key::MAIN_PHASE),
        ] {
            prepare_pair(
                &mut phase.gpu_occlusion_culling_build_indexed_indirect_params,
                &mut phase.gpu_occlusion_culling_build_non_indexed_indirect_params,
                key | Key::OCCLUSION_CULLING,
            );
        }
    }
}
