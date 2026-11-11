use std::collections::{BTreeMap, HashMap, hash_map::Entry};
use std::mem::{Discriminant, discriminant};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::asset::{RenderAssetUsages, embedded_asset};
use bevy::camera::primitives::{Aabb, Frustum, Sphere};
use bevy::camera::visibility::{RenderLayers, VisibilitySystems};
use bevy::camera::{Exposure, RenderTarget};
use bevy::core_pipeline::Skybox;
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{
    Indices, MeshTag, MeshVertexAttribute, MeshVertexBufferLayoutRef, PrimitiveTopology,
};
use bevy::pbr::{Lightmap, MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, ColorWrites, Extent3d, Face, RenderPipelineDescriptor, ShaderSize, ShaderType,
    SpecializedMeshPipelineError, TextureDimension, TextureFormat, TextureViewDescriptor,
    TextureViewDimension, VertexFormat, encase::UniformBuffer,
};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use bevy::shader::{ShaderDefVal, ShaderRef};

use bevy_surf::bc_texture::{BcFormat, BcTexture};
use bevy_surf::course::{Course, CourseTexture, TextureAlpha, VisualMaterial, VisualVertex};
use bevy_surf::visual_opacity::VisualOpacity;
use bevy_surf::visual_visibility::VisualVisibility;

use crate::{
    online::Online,
    view::{BACKGROUND, Game, ViewCamera},
};

#[derive(Component)]
pub struct MapGeometry;

const SKY_LAYER: usize = 1;

// Bevy binds this attribute at the standard color location. The Surfd shader
// reads only x and y, which hold the blend weight and baked prop light index.
const SURFD_VERTEX_DATA: MeshVertexAttribute = MeshVertexAttribute {
    format: VertexFormat::Float32x2,
    ..Mesh::ATTRIBUTE_COLOR
};
const OPAQUE_WORLD_LIGHT_FLAG: u32 = 1 << 18;

#[derive(Component)]
pub struct MapSkyCamera;

pub struct SurfdMaterialPlugin;

impl Plugin for SurfdMaterialPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "surfd_material.wesl");
        embedded_asset!(app, "sky_occluder.wesl");
        app.add_plugins(MaterialPlugin::<SurfdMaterial>::default());
        app.add_plugins(MaterialPlugin::<SkyOccluderMaterial>::default());
        app.init_resource::<RenderLods>();
        app.add_systems(
            PostUpdate,
            update_visible_lods
                .after(VisibilitySystems::UpdateFrusta)
                .before(VisibilitySystems::VisibilityPropagate),
        );
        crate::online_world::register_avatar_materials(app);
    }
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
#[bind_group_data(bool)]
pub struct SkyOccluderMaterial {
    #[texture(0, dimension = "cube")]
    #[sampler(1)]
    cube: Option<Handle<Image>>,
}

impl From<&SkyOccluderMaterial> for bool {
    fn from(material: &SkyOccluderMaterial) -> Self {
        material.cube.is_some()
    }
}

impl Material for SkyOccluderMaterial {
    fn fragment_shader() -> ShaderRef {
        "embedded://bevy_surf/sky_occluder.wesl".into()
    }

    fn enable_shadows() -> bool {
        false
    }

    fn specialize(
        _: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _: &MeshVertexBufferLayoutRef,
        key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = Some(Face::Back);
        if !key.bind_group_data
            && let Some(fragment) = &mut descriptor.fragment
        {
            for target in fragment.targets.iter_mut().flatten() {
                target.write_mask = ColorWrites::empty();
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct MaterialStyle {
    world: bool,
    sky: bool,
    fade_range: Option<[f32; 2]>,
    opacity_effect: bool,
    full_fade: bool,
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
#[bind_group_data(SurfdPipelineKey)]
pub struct SurfdMaterial {
    #[uniform(0)]
    params: SurfdUniforms,
    #[texture(1)]
    #[sampler(2)]
    base: Handle<Image>,
    #[texture(3)]
    second: Option<Handle<Image>>,
    #[texture(4)]
    detail: Option<Handle<Image>>,
    #[texture(5)]
    lightmap: Option<Handle<Image>>,
    #[texture(6)]
    prop_light: Option<Handle<Image>>,
    #[texture(7)]
    normal: Option<Handle<Image>>,
    #[texture(8)]
    env_mask: Option<Handle<Image>>,
    #[texture(9, dimension = "cube")]
    #[sampler(10)]
    environment: Option<Handle<Image>>,
    alpha: AlphaMode,
    double_sided: bool,
    world_light_shader: WorldLightShader,
}

#[derive(ShaderType, Debug, Clone)]
struct SurfdUniforms {
    tint: Vec3,
    opacity: f32,
    env_tint: Vec3,
    alpha_cut: f32,
    env_shaping: Vec3,
    seamless_scale: f32,
    seamless_offset: Vec3,
    detail_factor: f32,
    fog_color: Vec3,
    fog_max: f32,
    detail_scale: Vec2,
    fog_range: Vec2,
    lighting_response: Vec2,
    prop_fade_range: Vec2,
    flags: u32,
}

#[derive(PartialEq, Eq, Hash)]
struct SurfdBindingKey {
    uniforms: [u8; SURFD_UNIFORM_BYTES],
    textures: [Option<AssetId<Image>>; 8],
    alpha: Discriminant<AlphaMode>,
    double_sided: bool,
    world_light_shader: WorldLightShader,
}

const SURFD_UNIFORM_BYTES: usize = SurfdUniforms::SHADER_SIZE.get() as usize;

impl SurfdMaterial {
    fn binding_key(&self) -> SurfdBindingKey {
        let mut uniforms = UniformBuffer::new([0; SURFD_UNIFORM_BYTES]);
        uniforms
            .write(&self.params)
            .expect("fixed-size Surfd shader uniforms");
        SurfdBindingKey {
            uniforms: uniforms.into_inner(),
            textures: [
                Some(self.base.id()),
                self.second.as_ref().map(Handle::id),
                self.detail.as_ref().map(Handle::id),
                self.lightmap.as_ref().map(Handle::id),
                self.prop_light.as_ref().map(Handle::id),
                self.normal.as_ref().map(Handle::id),
                self.env_mask.as_ref().map(Handle::id),
                self.environment.as_ref().map(Handle::id),
            ],
            // Mask thresholds are already stored in the uniform bytes above.
            alpha: discriminant(&self.alpha),
            double_sided: self.double_sided,
            world_light_shader: self.world_light_shader,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum WorldLightShader {
    Generic,
    SharedOpaque,
    SeparateOpaque,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfdPipelineKey {
    double_sided: bool,
    world_light_shader: WorldLightShader,
}

impl From<&SurfdMaterial> for SurfdPipelineKey {
    fn from(material: &SurfdMaterial) -> Self {
        Self {
            double_sided: material.double_sided,
            world_light_shader: material.world_light_shader,
        }
    }
}

impl Material for SurfdMaterial {
    fn vertex_shader() -> ShaderRef {
        "embedded://bevy_surf/surfd_material.wesl".into()
    }

    fn fragment_shader() -> ShaderRef {
        Self::vertex_shader()
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    fn alpha_mode(&self) -> AlphaMode {
        self.alpha
    }

    fn specialize(
        _: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _: &MeshVertexBufferLayoutRef,
        key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = (!key.bind_group_data.double_sided).then_some(Face::Back);
        let feature = match key.bind_group_data.world_light_shader {
            WorldLightShader::Generic => None,
            WorldLightShader::SharedOpaque => Some("SURFD_UNIFORM_OPAQUE_WORLD_LIGHT"),
            WorldLightShader::SeparateOpaque => Some("SURFD_OPAQUE_WORLD_LIGHT"),
        };
        if let (Some(fragment), Some(feature)) = (&mut descriptor.fragment, feature) {
            fragment
                .shader_defs
                .push(ShaderDefVal::Bool(feature.into(), true));
        }
        Ok(())
    }
}

#[derive(Default)]
struct Buffers {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    uv: Vec<[f32; 2]>,
    light_uv: Vec<[f32; 2]>,
}

#[derive(Default)]
struct IndexUpload {
    bytes: usize,
    saved_bytes: usize,
}

impl IndexUpload {
    fn indices(&mut self, indices: Vec<u32>, vertices: usize) -> Indices {
        // Imported meshes validate each index against the local vertex count.
        // Triangle lists can address all 65,536 vertices with 16-bit indices.
        if vertices <= usize::from(u16::MAX) + 1 {
            let bytes = indices.len() * size_of::<u16>();
            self.bytes += bytes;
            self.saved_bytes += bytes;
            Indices::U16(indices.into_iter().map(|index| index as u16).collect())
        } else {
            self.bytes += indices.len() * size_of::<u32>();
            Indices::U32(indices)
        }
    }
}

impl Buffers {
    fn into_mesh(self) -> Mesh {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, self.colors)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uv)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, self.light_uv)
    }
}

#[derive(Resource, Default)]
pub struct RenderLods {
    groups: Vec<LodGroup>,
    last_view: Option<(Vec3, Quat, f32, UVec2)>,
    visibility: MapVisibility,
    effects: MapOpacityEffects,
}

impl RenderLods {
    pub(crate) fn pvs_counters(&self) -> Option<PvsCounters> {
        self.visibility.pvs.as_ref()?.counters
    }
}

#[derive(Default)]
struct MapOpacityEffects {
    surfaces: Vec<OpacitySurface>,
    sky_groups: Vec<SkyFadeGroup>,
    last_view: Option<(Vec3, Vec3, f64)>,
}

struct OpacitySurface {
    entity: Entity,
    effect: VisualOpacity,
    factor: f32,
}

struct SkyFadeGroup {
    origins: Arc<[(Vec3, f32)]>,
    meshes: Vec<(Entity, Handle<SurfdMaterial>, Handle<SurfdMaterial>)>,
    full: bool,
}

impl MapOpacityEffects {
    fn update(&mut self, eye: Vec3, actor_center: Vec3, fov: f64, commands: &mut Commands) {
        let view = (eye, actor_center, fov);
        if self.last_view == Some(view) {
            return;
        }
        for surface in &mut self.surfaces {
            let factor = surface.effect.factor(eye, actor_center, fov);
            if surface.factor == factor {
                continue;
            }
            let mut entity = commands.entity(surface.entity);
            entity.insert(MeshTag::new(factor.to_bits()));
            if (surface.factor > 0.0) != (factor > 0.0) {
                entity.insert(if factor > 0.0 {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                });
            }
            surface.factor = factor;
        }
        if self
            .last_view
            .is_none_or(|(previous, _, _)| previous != eye)
        {
            for group in &mut self.sky_groups {
                let full = group
                    .origins
                    .iter()
                    .all(|&(origin, start)| origin.distance(eye) <= start);
                if group.full == full {
                    continue;
                }
                group.full = full;
                for (entity, faded, opaque) in &group.meshes {
                    commands.entity(*entity).insert(MeshMaterial3d(if full {
                        opaque.clone()
                    } else {
                        faded.clone()
                    }));
                }
            }
        }
        self.last_view = Some(view);
    }
}

#[derive(Default)]
struct MapVisibility {
    pvs: Option<PvsState>,
    fading_props: Vec<FadingProp>,
    last_position: Option<Vec3>,
    last_fade_rejections: Option<usize>,
}

struct PvsState {
    table: VisualVisibility,
    last_clusters: Option<[usize; 9]>,
    mask: Vec<u8>,
    valid: bool,
    targets: PvsTargets,
    counters: Option<PvsCounters>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct PvsCounters {
    pub(crate) transitions: usize,
    pub(crate) target_checks: usize,
    pub(crate) visibility_changes: usize,
    pub(crate) elapsed: Duration,
}

enum PvsTargets {
    Flat(Vec<PvsTarget>),
    Grouped(Vec<PvsGroup>),
}

struct PvsTarget {
    draw: DrawVisibility,
    clusters: Arc<[usize]>,
}

struct PvsGroup {
    clusters: Arc<[usize]>,
    draws: Vec<Entity>,
    visible: bool,
}

impl PvsTargets {
    fn add(&mut self, target: PvsTarget) {
        let Self::Flat(targets) = self else {
            unreachable!("PVS draws are registered before groups are built");
        };
        targets.push(target);
    }

    fn group(&mut self) {
        let Self::Flat(targets) = self else {
            return;
        };
        let mut indices = HashMap::<Arc<[usize]>, usize>::new();
        let mut groups = Vec::<PvsGroup>::new();
        for target in targets.drain(..) {
            let index = *indices
                .entry(target.clusters)
                .or_insert_with_key(|clusters| {
                    let index = groups.len();
                    groups.push(PvsGroup {
                        clusters: clusters.clone(),
                        draws: Vec::new(),
                        visible: target.draw.visible,
                    });
                    index
                });
            groups[index].draws.push(target.draw.entity);
        }
        *self = Self::Grouped(groups);
    }

    fn len(&self) -> usize {
        match self {
            Self::Flat(targets) => targets.len(),
            Self::Grouped(groups) => groups.len(),
        }
    }

    fn update(
        &mut self,
        table: &VisualVisibility,
        mask: Option<&[u8]>,
        commands: &mut Commands,
    ) -> usize {
        let mut changed = 0;
        match self {
            Self::Flat(targets) => {
                for target in targets {
                    let visible =
                        mask.is_none_or(|mask| table.target_visible(mask, &target.clusters));
                    changed += usize::from(target.draw.set(visible, commands));
                }
            }
            Self::Grouped(groups) => {
                for group in groups {
                    let visible =
                        mask.is_none_or(|mask| table.target_visible(mask, &group.clusters));
                    if group.visible != visible {
                        group.visible = visible;
                        let visibility = draw_visibility(visible);
                        for &entity in &group.draws {
                            commands.entity(entity).insert(visibility);
                        }
                        changed += group.draws.len();
                    }
                }
            }
        }
        changed
    }
}

struct FadingProp {
    draw: DrawVisibility,
    clusters: Option<Arc<[usize]>>,
    pvs_visible: bool,
    distance: FadeDistance,
}

struct DrawVisibility {
    entity: Entity,
    visible: bool,
}

impl DrawVisibility {
    fn set(&mut self, visible: bool, commands: &mut Commands) -> bool {
        if self.visible == visible {
            return false;
        }
        self.visible = visible;
        commands
            .entity(self.entity)
            .insert(draw_visibility(visible));
        true
    }
}

fn draw_visibility(visible: bool) -> Visibility {
    if visible {
        Visibility::Visible
    } else {
        Visibility::Hidden
    }
}

struct FadeDistance {
    origin: Vec3,
    limit_squared: f32,
}

impl FadeDistance {
    fn new(origin: Vec3, [start, end]: [f32; 2]) -> Option<Self> {
        // Stay past the shader's zero-opacity boundary to account for CPU/GPU
        // distance rounding. Keep the shader fade for everything near the range.
        let end_with_margin = end.max(0.0) + (end.abs() / 4096.0).max(1.0);
        let limit_squared = end_with_margin * end_with_margin;
        (origin.is_finite()
            && end > start
            && (end - start).is_finite()
            && limit_squared.is_finite())
        .then_some(Self {
            origin,
            limit_squared,
        })
    }

    fn rejects(&self, camera: Vec3) -> bool {
        self.origin.distance_squared(camera) > self.limit_squared
    }
}

impl MapVisibility {
    fn track(
        &mut self,
        entity: Entity,
        clusters: Option<Arc<[usize]>>,
        distance: Option<FadeDistance>,
    ) {
        let draw = DrawVisibility {
            entity,
            visible: true,
        };
        if let Some(distance) = distance {
            self.fading_props.push(FadingProp {
                draw,
                clusters,
                pvs_visible: true,
                distance,
            });
        } else if let (Some(pvs), Some(clusters)) = (&mut self.pvs, clusters) {
            pvs.targets.add(PvsTarget { draw, clusters });
        }
    }

    fn update(&mut self, position: Vec3, commands: &mut Commands) {
        if self.last_position == Some(position)
            || (self.fading_props.is_empty()
                && self.pvs.as_ref().is_none_or(|pvs| pvs.targets.len() == 0))
        {
            return;
        }
        self.last_position = Some(position);
        let pvs_changed = self
            .pvs
            .as_mut()
            .is_some_and(|pvs| pvs.update(position, commands));
        let finite = position.is_finite();
        let mut rejected = 0;
        for prop in &mut self.fading_props {
            if pvs_changed && let (Some(pvs), Some(clusters)) = (&self.pvs, &prop.clusters) {
                prop.pvs_visible = pvs.visible(clusters);
            }
            let faded = finite && prop.pvs_visible && prop.distance.rejects(position);
            rejected += usize::from(faded);
            prop.draw.set(prop.pvs_visible && !faded, commands);
        }
        if !self.fading_props.is_empty() && self.last_fade_rejections != Some(rejected) {
            self.last_fade_rejections = Some(rejected);
            debug!(
                fade_candidates = self.fading_props.len(),
                fade_rejected = rejected,
                "Culled fully faded prop draws before frustum testing"
            );
        }
    }
}

impl PvsState {
    fn new(table: VisualVisibility, benchmark: bool) -> Self {
        Self {
            table,
            last_clusters: None,
            mask: Vec::new(),
            valid: false,
            targets: PvsTargets::Flat(Vec::new()),
            counters: benchmark.then(PvsCounters::default),
        }
    }

    fn update(&mut self, position: Vec3, commands: &mut Commands) -> bool {
        let clusters = self.table.clusters_near(position);
        if self.last_clusters == clusters {
            return false;
        }
        let started = self.counters.is_some().then(Instant::now);
        self.last_clusters = clusters;
        self.valid =
            clusters.is_some_and(|clusters| self.table.merge_rows(&clusters, &mut self.mask));
        let changed = self.targets.update(
            &self.table,
            self.valid.then_some(self.mask.as_slice()),
            commands,
        );
        if let (Some(counters), Some(started)) = (&mut self.counters, started) {
            counters.transitions += 1;
            counters.target_checks += self.targets.len();
            counters.visibility_changes += changed;
            counters.elapsed += started.elapsed();
        }
        true
    }

    fn visible(&self, clusters: &[usize]) -> bool {
        !self.valid || self.table.target_visible(&self.mask, clusters)
    }
}

#[derive(Clone)]
struct LodPart {
    mesh: Handle<Mesh>,
    material: Handle<SurfdMaterial>,
    light_offset: u32,
    bounds: Aabb,
}

impl LodPart {
    fn components(&self) -> (Mesh3d, MeshMaterial3d<SurfdMaterial>, MeshTag, Aabb) {
        (
            Mesh3d(self.mesh.clone()),
            MeshMaterial3d(self.material.clone()),
            MeshTag::new(self.light_offset),
            self.bounds,
        )
    }
}

struct LodGroup {
    transform: Transform,
    switch_points: Arc<[f32]>,
    depth_scale: f32,
    radius: f32,
    clusters: Option<Arc<[usize]>>,
    sky: bool,
    parts: Vec<Vec<LodPart>>,
    entities: Vec<Entity>,
    active: usize,
}

impl LodGroup {
    fn spawn(&mut self, commands: &mut Commands, visibility: &mut MapVisibility) {
        let slots = self.parts.iter().map(Vec::len).max().unwrap_or_default();
        for slot in 0..slots {
            let mut entity = commands.spawn((MapGeometry, self.transform));
            if self.sky {
                entity.insert(RenderLayers::layer(SKY_LAYER));
            }
            if let Some(part) = self.parts[0].get(slot) {
                entity.insert(part.components());
            }
            let id = entity.id();
            self.entities.push(id);
            visibility.track(id, self.clusters.clone(), None);
        }
    }
}

pub fn update_map_view(
    camera: Single<(&Transform, &Projection), With<ViewCamera>>,
    mut sky: Query<(&mut Transform, &mut Projection), (With<MapSkyCamera>, Without<ViewCamera>)>,
    game: Res<Game>,
    online: Res<Online>,
    mut lods: ResMut<RenderLods>,
    mut commands: Commands,
) {
    let (transform, projection) = camera.into_inner();
    if let Ok((mut sky_transform, mut sky_projection)) = sky.single_mut() {
        sky_transform.set_if_neq(*transform);
        if let (Projection::Perspective(main), Projection::Perspective(sky)) =
            (projection, &mut *sky_projection)
            && main.fov != sky.fov
        {
            sky.fov = main.fov;
        }
    }
    lods.visibility.update(transform.translation, &mut commands);
    if !lods.effects.surfaces.is_empty() || !lods.effects.sky_groups.is_empty() {
        let (feet, crouched) = online
            .viewed_pose()
            .filter(|_| online.show_players(&game))
            .map_or_else(
                || game.visual_actor_pose(),
                |player| (Vec3::from_array(player.position), player.crouched),
            );
        let center = feet + Vec3::Y * if crouched { 27.0 } else { 36.0 };
        let fov = if let Projection::Perspective(perspective) = projection {
            2.0 * ((f64::from(perspective.fov) * 0.5).tan() / 0.75)
                .atan()
                .to_degrees()
        } else {
            90.0
        };
        lods.effects
            .update(transform.translation, center, fov, &mut commands);
    }
}

fn update_visible_lods(
    camera: Single<(&Transform, &Projection, &Camera, &Frustum), With<ViewCamera>>,
    sky: Query<&Frustum, With<MapSkyCamera>>,
    benchmark: Option<Res<crate::bench::FrameBenchmark>>,
    mut benchmark_all_lods: Local<Option<bool>>,
    mut lods: ResMut<RenderLods>,
    mut commands: Commands,
) {
    if lods.groups.is_empty() {
        return;
    }
    let (transform, projection, camera, frustum) = camera.into_inner();
    let (Projection::Perspective(projection), Some(size)) =
        (projection, camera.physical_viewport_size())
    else {
        return;
    };
    if size.y == 0 {
        return;
    }
    let position = transform.translation;
    let forward = *transform.forward();
    let scale = 100.0 * (projection.fov * 0.5).tan() / size.y as f32;
    let view = (position, transform.rotation, scale, size);
    if lods.last_view == Some(view) {
        return;
    }
    lods.last_view = Some(view);
    let all_lods = benchmark.is_some()
        && *benchmark_all_lods.get_or_insert_with(|| {
            std::env::var("SURF_BENCH_ALL_LODS").is_ok_and(|value| value == "1")
        });
    let sky_frustum = sky.single().ok();
    for group in &mut lods.groups {
        let depth = (group.transform.translation - position).dot(forward);
        if depth < -group.radius {
            continue;
        }
        let metric = depth.max(0.0) * scale / group.depth_scale;
        let level = group
            .switch_points
            .iter()
            .position(|&point| point > metric)
            .unwrap_or(group.switch_points.len());
        if group.active == level {
            continue;
        }
        let group_frustum = if group.sky {
            sky_frustum
        } else {
            Some(frustum)
        };
        if !all_lods
            && group_frustum.is_some_and(|frustum| {
                // The radius encloses every level, including the mesh still on the entity.
                !frustum.intersects_sphere(
                    &Sphere {
                        center: group.transform.translation.into(),
                        radius: group.radius + 0.05,
                    },
                    false,
                )
            })
        {
            continue;
        }
        group.active = level;
        for (slot, &entity) in group.entities.iter().enumerate() {
            let mut entity = commands.entity(entity);
            if let Some(part) = group.parts[level].get(slot) {
                entity.insert(part.components());
            } else {
                entity.remove::<(Mesh3d, MeshMaterial3d<SurfdMaterial>, MeshTag, Aabb)>();
            }
        }
    }
}

pub fn update_world(
    mut commands: Commands,
    mut game: ResMut<Game>,
    camera: Single<
        (
            Entity,
            &mut DistanceFog,
            &mut Camera,
            &mut Projection,
            &Transform,
            &RenderTarget,
            &Msaa,
            Option<&Exposure>,
        ),
        With<ViewCamera>,
    >,
    old_geometry: Query<Entity, With<MapGeometry>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut surfd_materials: ResMut<Assets<SurfdMaterial>>,
    mut occluder_materials: ResMut<Assets<SkyOccluderMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut lods: ResMut<RenderLods>,
    benchmark: Option<Res<crate::bench::FrameBenchmark>>,
    mut rendered: Local<Option<u64>>,
) {
    if *rendered == Some(game.course_version) {
        return;
    }
    let prepare_started = Instant::now();
    *rendered = Some(game.course_version);
    for entity in &old_geometry {
        commands.entity(entity).despawn();
    }

    let (camera, mut fog, mut camera_config, mut projection, transform, target, msaa, exposure) =
        camera.into_inner();
    let course = &mut game.course;
    let packed_meshes = std::mem::take(&mut course.visual_meshes);
    let sky_occluders = std::mem::take(&mut course.sky_occluders);
    let mut index_upload = IndexUpload::default();
    let wide_vertices = benchmark.is_some()
        && std::env::var("SURF_BENCH_WIDE_VERTICES").is_ok_and(|value| value == "1");
    let copy_texture_uploads = benchmark.is_some()
        && std::env::var("SURF_BENCH_COPY_TEXTURE_UPLOADS").is_ok_and(|value| value == "1");
    let ungrouped_pvs = benchmark.is_some()
        && std::env::var("SURF_BENCH_UNGROUPED_PVS").is_ok_and(|value| value == "1");
    let world_light_shader = if benchmark.is_some()
        && std::env::var("SURF_BENCH_GENERIC_MATERIALS").is_ok_and(|value| value == "1")
    {
        WorldLightShader::Generic
    } else if benchmark.is_some()
        && std::env::var("SURF_BENCH_SPECIALIZED_MATERIALS").is_ok_and(|value| value == "1")
    {
        WorldLightShader::SeparateOpaque
    } else if benchmark.is_some()
        && std::env::var("SURF_BENCH_SHARED_MATERIALS").is_ok_and(|value| value == "1")
        && packed_meshes.iter().any(|source| {
            source.instances.is_empty()
                && source.opacity.is_none()
                && uses_opaque_world_light(&source.material, source.material.environment.is_some())
        })
    {
        WorldLightShader::SharedOpaque
    } else {
        WorldLightShader::Generic
    };
    let mut surfd_vertices = 0;
    let has_3d_sky = packed_meshes.iter().any(|mesh| {
        if mesh.instances.is_empty() {
            mesh.sky
        } else {
            mesh.instances.iter().any(|instance| instance.sky)
        }
    });
    if let Projection::Perspective(projection) = &mut *projection {
        (projection.near, projection.far) = if packed_meshes.is_empty() {
            (1.0, 65_536.0)
        } else {
            (2.0, 200_000.0)
        };
    }
    let mut visibility = MapVisibility {
        pvs: course
            .visual_visibility
            .take()
            .map(|table| PvsState::new(table, benchmark.is_some())),
        ..default()
    };
    let mut lod_groups = BTreeMap::new();
    let mut sky_fade_groups = BTreeMap::new();
    let mut effects = MapOpacityEffects::default();
    let clear_color = course
        .fog
        .as_ref()
        .map_or(ClearColorConfig::Default, |fog| {
            let [red, green, blue] = fog.color;
            ClearColorConfig::Custom(Color::srgb(red, green, blue))
        });
    *fog = match &course.fog {
        Some(fog) => DistanceFog {
            color: Color::srgba(fog.color[0], fog.color[1], fog.color[2], fog.max_density),
            falloff: FogFalloff::Linear {
                start: fog.start,
                end: fog.end,
            },
            ..default()
        },
        None => DistanceFog {
            color: BACKGROUND,
            falloff: FogFalloff::Linear {
                start: 1450.,
                end: if course.textures.is_empty() {
                    6000.
                } else {
                    24_000.
                },
            },
            ..default()
        },
    };
    let cube = course
        .sky
        .and_then(|faces| cube_map(course, faces))
        .map(|image| images.add(image));
    let exposure = exposure.copied().unwrap_or_default();
    let skybox = cube.as_ref().map(|image| Skybox {
        image: Some(image.clone()),
        brightness: exposure.exposure().recip(),
        ..default()
    });
    if has_3d_sky {
        camera_config.clear_color = ClearColorConfig::None;
        commands.entity(camera).remove::<Skybox>();
        let mut sky = commands.spawn((
            MapGeometry,
            MapSkyCamera,
            Camera3d::default(),
            Camera {
                order: camera_config.order - 1,
                viewport: camera_config.viewport.clone(),
                clear_color,
                ..default()
            },
            target.clone(),
            projection.clone(),
            *transform,
            *msaa,
            exposure,
            Tonemapping::None,
            DebandDither::Disabled,
            RenderLayers::layer(SKY_LAYER),
        ));
        if let Some(skybox) = skybox {
            sky.insert(skybox);
        }
    } else {
        camera_config.clear_color = clear_color;
        if let Some(skybox) = skybox {
            commands.entity(camera).insert(skybox);
        } else {
            commands.entity(camera).remove::<Skybox>();
        }
    }

    if sky_occluders.is_empty() {
        commands.entity(camera).remove::<DepthPrepass>();
    } else {
        commands.entity(camera).insert(DepthPrepass);
        let material = occluder_materials.add(SkyOccluderMaterial {
            cube: if has_3d_sky { None } else { cube },
        });
        for occluder in sky_occluders {
            let Some(bounds) = Aabb::enclosing(&occluder.positions) else {
                continue;
            };
            let indices = index_upload.indices(occluder.indices, occluder.positions.len());
            let mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::RENDER_WORLD,
            )
            .with_inserted_attribute(
                Mesh::ATTRIBUTE_POSITION,
                occluder
                    .positions
                    .into_iter()
                    .map(|position| position.to_array())
                    .collect::<Vec<_>>(),
            )
            .with_inserted_indices(indices);
            commands.spawn((
                MapGeometry,
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material.clone()),
                Transform::IDENTITY,
                bounds,
            ));
        }
    }

    let mut handles = MaterialBank {
        course,
        images: &mut images,
        materials: &mut materials,
        textures: BTreeMap::new(),
        by_texture: BTreeMap::new(),
        cubes: BTreeMap::new(),
        white: None,
        world_light_shader,
    };
    for ((base, light), buffers) in polygon_batches(course) {
        let mesh = meshes.add(buffers.into_mesh());
        let material = handles.material(base, light.is_some());
        let lightmap = handles.lightmap(light);
        spawn_mesh(&mut commands, mesh, material, lightmap, Transform::IDENTITY);
    }
    let mut material_bindings = HashMap::<SurfdBindingKey, Handle<SurfdMaterial>>::new();
    let mut reused_material_bindings = 0;
    let mut opaque_world_materials = 0;
    let mut shared_opaque_materials = 0;
    for source in packed_meshes {
        let Some(bounds) = Aabb::enclosing(source.vertices.iter().map(|vertex| &vertex.position))
        else {
            continue;
        };
        let radius = (bounds.center.abs() + bounds.half_extents).length();
        surfd_vertices += source.vertices.len();
        let indices = index_upload.indices(source.indices, source.vertices.len());
        let mesh = meshes.add(indexed_mesh(&source.vertices, indices, wide_vertices));
        let mut variants = BTreeMap::new();
        let mut material = |environment, style: MaterialStyle| {
            let key = (
                environment,
                style.sky,
                style.fade_range.map(|range| range.map(f32::to_bits)),
                style.opacity_effect,
                style.full_fade,
            );
            variants
                .entry(key)
                .or_insert_with(|| {
                    let material = handles.surfd(&source.material, environment, style);
                    match material_bindings.entry(material.binding_key()) {
                        Entry::Occupied(entry) => {
                            reused_material_bindings += 1;
                            entry.get().clone()
                        }
                        Entry::Vacant(entry) => {
                            opaque_world_materials += usize::from(
                                material.world_light_shader == WorldLightShader::SeparateOpaque
                                    || material.params.flags & OPAQUE_WORLD_LIGHT_FLAG != 0,
                            );
                            shared_opaque_materials += usize::from(
                                material.world_light_shader == WorldLightShader::SharedOpaque,
                            );
                            entry.insert(surfd_materials.add(material)).clone()
                        }
                    }
                })
                .clone()
        };
        if source.instances.is_empty() {
            let style = MaterialStyle {
                world: true,
                sky: source.sky,
                opacity_effect: source.opacity.is_some(),
                ..default()
            };
            let mut entity = commands.spawn((
                MapGeometry,
                Mesh3d(mesh),
                MeshMaterial3d(material(source.material.environment, style)),
                Transform::IDENTITY,
                bounds,
            ));
            if source.sky {
                entity.insert(RenderLayers::layer(SKY_LAYER));
            }
            if let Some(effect) = source.opacity {
                entity.insert(MeshTag::new(1.0_f32.to_bits()));
                effects.surfaces.push(OpacitySurface {
                    entity: entity.id(),
                    effect,
                    factor: 1.0,
                });
            }
            visibility.track(entity.id(), source.clusters, None);
        } else {
            for instance in source.instances {
                let transform = Transform::from_matrix(instance.transform);
                let style = MaterialStyle {
                    sky: instance.sky,
                    fade_range: instance.fade.as_ref().map(|fade| fade.range),
                    ..default()
                };
                let environment = instance.environment.or(source.material.environment);
                let part = LodPart {
                    mesh: mesh.clone(),
                    material: material(environment, style),
                    light_offset: instance.light_offset.unwrap_or(u32::MAX),
                    bounds,
                };
                if let Some(lod) = instance.lod {
                    let group = lod_groups.entry(lod.prop).or_insert_with(|| LodGroup {
                        transform,
                        parts: vec![Vec::new(); lod.switch_points.len() + 1],
                        switch_points: lod.switch_points,
                        depth_scale: lod.depth_scale,
                        radius: 0.0,
                        clusters: instance.clusters,
                        sky: instance.sky,
                        entities: Vec::new(),
                        active: 0,
                    });
                    group.radius = group
                        .radius
                        .max(radius * transform.scale.abs().max_element());
                    group.parts[lod.level].push(part);
                } else {
                    let mut entity = commands.spawn((MapGeometry, transform, part.components()));
                    if instance.sky {
                        entity.insert(RenderLayers::layer(SKY_LAYER));
                    }
                    let entity = entity.id();
                    if !source.material.translucent
                        && let Some(origins) = instance.fade.and_then(|fade| fade.sky_group)
                    {
                        let opaque = material(
                            environment,
                            MaterialStyle {
                                full_fade: true,
                                ..style
                            },
                        );
                        let key = Arc::as_ptr(&origins).cast::<(Vec3, f32)>() as usize;
                        let group = sky_fade_groups.entry(key).or_insert_with(|| SkyFadeGroup {
                            origins,
                            meshes: Vec::new(),
                            full: false,
                        });
                        group.meshes.push((entity, part.material, opaque));
                    }
                    let fade_distance = style
                        .fade_range
                        .and_then(|range| FadeDistance::new(transform.translation, range));
                    visibility.track(entity, instance.clusters, fade_distance);
                }
            }
        }
    }
    lods.groups = lod_groups.into_values().collect();
    lods.last_view = None;
    for group in &mut lods.groups {
        group.spawn(&mut commands, &mut visibility);
    }
    let (pvs_draws, pvs_targets) = visibility.pvs.as_mut().map_or((0, 0), |pvs| {
        let draws = pvs.targets.len();
        if !ungrouped_pvs {
            pvs.targets.group();
        }
        (draws, pvs.targets.len())
    });
    lods.visibility = visibility;
    effects.sky_groups = sky_fade_groups.into_values().collect();
    lods.effects = effects;
    let pending_textures = handles.textures;
    let texture_uploads = upload_texture_payloads(
        &mut course.textures,
        &mut images,
        pending_textures,
        copy_texture_uploads,
    );
    debug!(
        map = %course.name,
        surfd_vertices,
        surfd_vertex_bytes_saved = if wide_vertices { 0 } else { surfd_vertices * size_of::<[f32; 2]>() },
        index_bytes = index_upload.bytes,
        index_bytes_saved = index_upload.saved_bytes,
        material_bindings = material_bindings.len(),
        reused_material_bindings,
        opaque_world_materials,
        shared_opaque_materials,
        pvs_draws,
        pvs_targets,
        texture_bytes_copied = texture_uploads.copied,
        texture_rgba_bytes_transferred = texture_uploads.rgba_transferred,
        texture_bc_bytes_transferred = texture_uploads.bc_transferred,
        texture_payload_ms = texture_uploads.elapsed_ms,
        render_prep_ms = prepare_started.elapsed().as_secs_f64() * 1000.0,
        "Prepared map render assets"
    );
    // The GPU assets now own the render data. Retain collision and run metadata.
    course.surfaces = Vec::new();
    course.textures = Vec::new();
    course.sky = None;
}

type MaterialKey = (Option<usize>, Option<usize>);

fn polygon_batches(course: &Course) -> BTreeMap<MaterialKey, Buffers> {
    let mut batches = BTreeMap::<_, Buffers>::new();
    for surface in &course.surfaces {
        let base = surface
            .texture
            .as_ref()
            .filter(|texture| texture.uvs.len() == surface.vertices.len());
        let light = surface
            .lightmap
            .as_ref()
            .filter(|light| light.uvs.len() == surface.vertices.len());
        let buffers = batches
            .entry((
                base.map(|texture| texture.index),
                light.map(|light| light.index),
            ))
            .or_default();
        let [red, green, blue] = surface.color;
        let color = Color::srgb(red, green, blue).to_linear();
        for index in 1..surface.vertices.len().saturating_sub(1) {
            let first = surface.vertices[0];
            let normal = (surface.vertices[index] - first)
                .cross(surface.vertices[index + 1] - first)
                .normalize_or_zero();
            if normal == Vec3::ZERO {
                continue;
            }
            let shade = 0.58 + 0.42 * normal.dot(Vec3::new(0.23, 0.92, 0.31)).max(0.0);
            let tint = if base.is_some() {
                [1.0; 4]
            } else {
                [
                    color.red * shade,
                    color.green * shade,
                    color.blue * shade,
                    1.0,
                ]
            };
            for i in [0, index, index + 1] {
                buffers.positions.push(surface.vertices[i].to_array());
                buffers.normals.push(normal.to_array());
                buffers.colors.push(tint);
                buffers
                    .uv
                    .push(base.map_or([0.; 2], |texture| texture.uvs[i]));
                buffers
                    .light_uv
                    .push(light.map_or([0.; 2], |light| light.uvs[i]));
            }
        }
    }
    batches
}

fn indexed_mesh(vertices: &[VisualVertex], indices: Indices, wide_vertices: bool) -> Mesh {
    let mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vertices
            .iter()
            .map(|v| v.position.to_array())
            .collect::<Vec<_>>(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vertices
            .iter()
            .map(|v| v.normal.to_array())
            .collect::<Vec<_>>(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vertices.iter().map(|v| v.uv).collect::<Vec<_>>(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_UV_1,
        vertices.iter().map(|v| v.light_uv).collect::<Vec<_>>(),
    )
    .with_inserted_indices(indices);
    if wide_vertices {
        mesh.with_inserted_attribute(
            Mesh::ATTRIBUTE_COLOR,
            vertices
                .iter()
                .map(|v| [v.blend, v.light_index, 0.0, 1.0])
                .collect::<Vec<_>>(),
        )
    } else {
        mesh.with_inserted_attribute(
            SURFD_VERTEX_DATA,
            vertices
                .iter()
                .map(|v| [v.blend, v.light_index])
                .collect::<Vec<_>>(),
        )
    }
}

fn spawn_mesh(
    commands: &mut Commands,
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
    light: Option<Lightmap>,
    transform: Transform,
) {
    let mut entity = commands.spawn((
        MapGeometry,
        Mesh3d(mesh),
        MeshMaterial3d(material),
        transform,
    ));
    if let Some(light) = light {
        entity.insert(light);
    }
}

struct MaterialBank<'a> {
    course: &'a Course,
    images: &'a mut Assets<Image>,
    materials: &'a mut Assets<StandardMaterial>,
    textures: BTreeMap<(usize, TextureUsage), Handle<Image>>,
    by_texture: BTreeMap<(Option<usize>, bool), Handle<StandardMaterial>>,
    cubes: BTreeMap<[usize; 6], Handle<Image>>,
    white: Option<Handle<Image>>,
    world_light_shader: WorldLightShader,
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum TextureUsage {
    Color,
    Data,
    Baked,
}

fn uses_opaque_world_light(material: &VisualMaterial, has_environment: bool) -> bool {
    !material.translucent
        && !material.additive
        && material.alpha_cut.is_none()
        && !material.vertex_alpha
        && material.lightmap.is_some()
        && material.rgb_exp_lightmap
        && material.prop_light.is_none()
        && !material.unlit
        && !material.reflection_only
        && !has_environment
        && material.second.is_none()
        && material.detail.is_none()
        && material.seamless == 0.0
}

impl MaterialBank<'_> {
    fn image(&mut self, index: usize, usage: TextureUsage) -> Option<Handle<Image>> {
        self.course.textures.get(index)?;
        Some(
            self.textures
                .entry((index, usage))
                .or_insert_with(|| self.images.reserve_handle())
                .clone(),
        )
    }

    fn material(&mut self, base: Option<usize>, lit: bool) -> Handle<StandardMaterial> {
        if let Some(material) = self.by_texture.get(&(base, lit)) {
            return material.clone();
        }
        let image = base.and_then(|index| self.image(index, TextureUsage::Color));
        let alpha_mode = base
            .and_then(|index| self.course.textures.get(index))
            .map_or(AlphaMode::Opaque, |texture| match texture.alpha {
                TextureAlpha::Opaque => AlphaMode::Opaque,
                TextureAlpha::Cutout => AlphaMode::Mask(0.5),
                TextureAlpha::Blend => AlphaMode::Blend,
            });
        let material = self.materials.add(StandardMaterial {
            base_color_texture: image,
            alpha_mode,
            cull_mode: None,
            unlit: !lit,
            perceptual_roughness: 1.0,
            reflectance: 0.0,
            ..default()
        });
        self.by_texture.insert((base, lit), material.clone());
        material
    }

    fn lightmap(&mut self, index: Option<usize>) -> Option<Lightmap> {
        Some(Lightmap {
            image: self.image(index?, TextureUsage::Baked)?,
            uv_rect: Rect::new(0., 0., 1., 1.),
            bicubic_sampling: false,
        })
    }

    fn surfd(
        &mut self,
        material: &VisualMaterial,
        environment: Option<[usize; 6]>,
        style: MaterialStyle,
    ) -> SurfdMaterial {
        use TextureUsage::{Baked, Color as Srgb, Data};

        let base = material
            .base
            .and_then(|index| self.image(index, Srgb))
            .unwrap_or_else(|| {
                self.white
                    .get_or_insert_with(|| {
                        let mut white = Image::default();
                        white.sampler = repeat_sampler();
                        self.images.add(white)
                    })
                    .clone()
            });
        let environment = environment.and_then(|faces| {
            if let Some(cached) = self.cubes.get(&faces) {
                return Some(cached.clone());
            }
            let cube = self.images.add(cube_map(self.course, faces)?);
            self.cubes.insert(faces, cube.clone());
            Some(cube)
        });
        let fog = self.course.fog.as_ref().filter(|_| !material.no_fog);
        let translucent = material.translucent
            || style.opacity_effect
            || (style.fade_range.is_some() && !style.full_fade);
        let alpha = if material.additive {
            AlphaMode::Add
        } else if translucent {
            AlphaMode::Blend
        } else if let Some(cut) = material.alpha_cut {
            AlphaMode::Mask(cut)
        } else {
            AlphaMode::Opaque
        };
        let opaque_world_light = style.world
            && !style.opacity_effect
            && style.fade_range.is_none()
            && uses_opaque_world_light(material, environment.is_some());
        let world_light_shader = match self.world_light_shader {
            // The material flag leaves the pipeline key and opaque bin order
            // identical for eligible world surfaces and other opaque draws.
            WorldLightShader::SharedOpaque if matches!(alpha, AlphaMode::Opaque) => {
                WorldLightShader::SharedOpaque
            }
            WorldLightShader::SeparateOpaque if opaque_world_light => {
                WorldLightShader::SeparateOpaque
            }
            _ => WorldLightShader::Generic,
        };
        let flags = [
            material.second.is_some(),
            material.detail.is_some(),
            material.alpha_cut.is_some(),
            material.vertex_alpha,
            translucent,
            material.lightmap.is_some(),
            material.rgb_exp_lightmap,
            material.prop_light.is_some(),
            material.unlit,
            material.preserve_shadow_color && !style.sky,
            material.reflection_only,
            environment.is_some(),
            material.apply_normal,
            material.normal_alpha_mask,
            material.env_mask.is_some(),
            material.env_shaping.is_some(),
            style.fade_range.is_some(),
            style.opacity_effect,
        ]
        .into_iter()
        .enumerate()
        .fold(0, |flags, (bit, enabled)| {
            flags | (u32::from(enabled) << bit)
        });
        let rgb_only = |index: Option<usize>| {
            index
                .and_then(|index| self.course.textures.get(index))
                .and_then(|texture| texture.bc.as_ref())
                .is_some_and(|texture| texture.rgb_only)
        };
        let flags = flags
            | (if world_light_shader == WorldLightShader::SharedOpaque && opaque_world_light {
                OPAQUE_WORLD_LIGHT_FLAG
            } else {
                0
            })
            | (u32::from(rgb_only(material.base)) << 24)
            | (u32::from(rgb_only(material.second)) << 25)
            | (u32::from(rgb_only(material.normal)) << 26);
        let (sky_origin, sky_scale) = material.sky_camera.unwrap_or((Vec3::ZERO, 1.0));
        SurfdMaterial {
            params: SurfdUniforms {
                tint: Vec3::from_array(material.tint),
                opacity: material.opacity,
                env_tint: Vec3::from_array(material.env_tint),
                alpha_cut: material.alpha_cut.unwrap_or_default(),
                env_shaping: Vec3::from_array(material.env_shaping.unwrap_or([0.0, 1.0, 1.0])),
                seamless_scale: material.seamless / sky_scale,
                seamless_offset: sky_origin * material.seamless,
                detail_factor: material.detail_factor,
                fog_color: fog.map_or(Vec3::ZERO, |fog| {
                    let [r, g, b] = fog.color;
                    let linear = Color::srgb(r, g, b).to_linear();
                    Vec3::new(linear.red, linear.green, linear.blue)
                }),
                fog_max: fog.map_or(0.0, |fog| fog.max_density),
                detail_scale: Vec2::from_array(material.detail_scale),
                fog_range: fog.map_or(Vec2::Y, |fog| Vec2::new(fog.start, fog.end)),
                lighting_response: if style.sky {
                    Vec2::X
                } else {
                    Vec2::from_array(material.lighting_response.unwrap_or([1.0, 0.0]))
                },
                prop_fade_range: style.fade_range.map_or(Vec2::ZERO, Vec2::from_array),
                flags,
            },
            base,
            second: material.second.and_then(|index| self.image(index, Srgb)),
            detail: material.detail.and_then(|index| self.image(index, Data)),
            lightmap: material.lightmap.and_then(|index| self.image(index, Baked)),
            prop_light: material
                .prop_light
                .and_then(|index| self.image(index, Srgb)),
            normal: material.normal.and_then(|index| self.image(index, Data)),
            env_mask: material.env_mask.and_then(|index| self.image(index, Data)),
            environment,
            alpha,
            double_sided: material.double_sided,
            world_light_shader,
        }
    }
}

fn repeat_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        ..ImageSamplerDescriptor::linear()
    })
}

#[derive(Default)]
struct TextureUploads {
    copied: usize,
    rgba_transferred: usize,
    bc_transferred: usize,
    elapsed_ms: f64,
}

fn upload_texture_payloads(
    textures: &mut [CourseTexture],
    images: &mut Assets<Image>,
    pending: BTreeMap<(usize, TextureUsage), Handle<Image>>,
    copy_all: bool,
) -> TextureUploads {
    let started = Instant::now();
    let mut stats = TextureUploads::default();
    let mut pending = pending.into_iter().peekable();
    while let Some(((index, usage), handle)) = pending.next() {
        let texture = &mut textures[index];
        let mut image = texture_image(texture, usage);
        // Cubes are already assembled. Share the source bytes across any 2D
        // format variants, then give the final variant the original allocation.
        let copied = copy_all || pending.peek().is_some_and(|((next, _), _)| *next == index);
        let bytes = if copied {
            let bytes = texture
                .bc
                .as_ref()
                .map_or(&texture.rgba[..], BcTexture::blocks);
            stats.copied += bytes.len();
            bytes.to_vec()
        } else if let Some(bc) = texture.bc.take() {
            let bytes = bc.into_blocks();
            stats.bc_transferred += bytes.len();
            bytes
        } else {
            stats.rgba_transferred += texture.rgba.len();
            std::mem::take(&mut texture.rgba)
        };
        image.data = Some(bytes);
        images
            .insert(handle.id(), image)
            .expect("reserved map texture handle");
    }
    stats.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    stats
}

fn texture_image(texture: &CourseTexture, usage: TextureUsage) -> Image {
    let size = Extent3d {
        width: texture.width,
        height: texture.height,
        depth_or_array_layers: 1,
    };
    let srgb = usage == TextureUsage::Color;
    let mut image = if let Some(bc) = &texture.bc {
        let format = match (bc.format, srgb) {
            (BcFormat::Bc1, false) => TextureFormat::Bc1RgbaUnorm,
            (BcFormat::Bc1, true) => TextureFormat::Bc1RgbaUnormSrgb,
            (BcFormat::Bc2, false) => TextureFormat::Bc2RgbaUnorm,
            (BcFormat::Bc2, true) => TextureFormat::Bc2RgbaUnormSrgb,
            (BcFormat::Bc3, false) => TextureFormat::Bc3RgbaUnorm,
            (BcFormat::Bc3, true) => TextureFormat::Bc3RgbaUnormSrgb,
        };
        let mut image = Image::new_uninit(
            size,
            TextureDimension::D2,
            format,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.texture_descriptor.mip_level_count = bc.mip_levels;
        image
    } else {
        debug_assert_eq!(
            texture.rgba.len(),
            texture.width as usize * texture.height as usize * 4,
        );
        Image::new_uninit(
            size,
            TextureDimension::D2,
            if srgb {
                TextureFormat::Rgba8UnormSrgb
            } else {
                TextureFormat::Rgba8Unorm
            },
            RenderAssetUsages::RENDER_WORLD,
        )
    };
    image.sampler = if usage == TextureUsage::Baked {
        ImageSampler::linear()
    } else {
        repeat_sampler()
    };
    image
}

fn cube_map(course: &Course, faces: [usize; 6]) -> Option<Image> {
    let first = course.textures.get(faces[0])?;
    if first.width != first.height {
        return None;
    }
    let mut bytes = Vec::with_capacity(first.rgba.len() * 6);
    for index in faces {
        let face = course.textures.get(index)?;
        if face.bc.is_some() || (face.width, face.height) != (first.width, first.height) {
            return None;
        }
        bytes.extend_from_slice(&face.rgba);
    }
    let mut image = Image::new(
        Extent3d {
            width: first.width,
            height: first.height,
            depth_or_array_layers: 6,
        },
        TextureDimension::D2,
        bytes,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..default()
    });
    image.sampler = ImageSampler::linear();
    Some(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::world::CommandQueue;

    #[test]
    fn grouped_pvs_matches_each_draw_through_mask_and_fallback_changes() {
        let table: VisualVisibility = serde_json::from_value(serde_json::json!({
            "format": 1, "bspSha1": "fixture", "clusterCount": 2,
            "planes": [[1.0, 0.0, 0.0, 0.0]], "nodes": [[0, -2, -3]],
            "pvs": ["AQ==", "Ag=="]
        }))
        .unwrap();
        let mut world = World::new();
        let mut setup = || {
            let entities =
                [0, 1, 0].map(|cluster| (world.spawn(Visibility::Visible).id(), cluster));
            let targets = entities
                .iter()
                .map(|&(entity, cluster)| PvsTarget {
                    draw: DrawVisibility {
                        entity,
                        visible: true,
                    },
                    clusters: Arc::from([cluster]),
                })
                .collect();
            (
                entities.map(|(entity, _)| entity),
                PvsTargets::Flat(targets),
            )
        };
        let (flat_entities, mut flat) = setup();
        let (grouped_entities, mut grouped) = setup();
        grouped.group();
        assert_eq!((flat.len(), grouped.len()), (3, 2));

        for mask in [Some(&[1][..]), Some(&[2][..]), Some(&[2][..]), None] {
            let mut queue = CommandQueue::default();
            let mut commands = Commands::new(&mut queue, &world);
            assert_eq!(
                flat.update(&table, mask, &mut commands),
                grouped.update(&table, mask, &mut commands),
            );
            queue.apply(&mut world);
            assert_eq!(
                flat_entities.map(|entity| world.get::<Visibility>(entity).copied()),
                grouped_entities.map(|entity| world.get::<Visibility>(entity).copied()),
            );
        }
    }

    #[test]
    fn texture_upload_keeps_both_formats_and_moves_the_last_payload() {
        let mut images = Assets::<Image>::default();
        let color = images.reserve_handle();
        let data = images.reserve_handle();
        let pixels = vec![37, 84, 169, 210];
        let allocation = pixels.as_ptr();
        let mut textures = [CourseTexture {
            name: "two usages".into(),
            width: 1,
            height: 1,
            rgba: pixels,
            bc: None,
            alpha: TextureAlpha::Opaque,
        }];
        let pending = BTreeMap::from([
            ((0, TextureUsage::Color), color.clone()),
            ((0, TextureUsage::Data), data.clone()),
        ]);

        let stats = upload_texture_payloads(&mut textures, &mut images, pending, false);

        assert_eq!(stats.copied, 4);
        assert_eq!(stats.rgba_transferred, 4);
        assert!(textures[0].rgba.is_empty());
        let color = images.get(&color).unwrap();
        let data = images.get(&data).unwrap();
        assert_eq!(
            color.texture_descriptor.format,
            TextureFormat::Rgba8UnormSrgb
        );
        assert_eq!(data.texture_descriptor.format, TextureFormat::Rgba8Unorm);
        assert_eq!(color.data.as_deref(), Some(&[37, 84, 169, 210][..]));
        assert_eq!(color.data, data.data);
        assert_eq!(data.data.as_ref().unwrap().as_ptr(), allocation);
    }
}
