use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, Weak},
};

use bevy_math::{DVec3, Mat4, Vec3};
use sha2::{Digest, Sha256};

use crate::{brush_index::BrushIndex, rules::PhysicsProfile};

#[path = "collision_planes.rs"]
mod collision_planes;
pub use collision_planes::BrushPlanes;
pub(crate) use collision_planes::{AuthoredZoneGeometry, DisplacementHull, PrecisePlane};

fn hash_vec(digest: &mut Sha256, value: Vec3) {
    for component in value.to_array() {
        digest.update(component.to_le_bytes());
    }
}

fn hash_brush(digest: &mut Sha256, brush: &Brush) {
    hash_vec(digest, brush.min);
    hash_vec(digest, brush.max);
    digest.update((brush.planes.len() as u64).to_le_bytes());
    for plane in &brush.planes {
        hash_vec(digest, plane.normal);
        digest.update(plane.distance.to_le_bytes());
    }
}

fn hash_filter(digest: &mut Sha256, filter: Option<&TriggerFilter>) {
    digest.update([u8::from(filter.is_some())]);
    if let Some(filter) = filter {
        digest.update((filter.targetname.len() as u64).to_le_bytes());
        digest.update(filter.targetname.as_bytes());
        digest.update([u8::from(filter.negated)]);
    }
}

/// Course geometry uses Source units, with Bevy's Y-up axes.
pub struct Course {
    pub name: String,
    /// Assigned by the trusted catalog loader after checking the geometry asset.
    pub catalog_revision: Option<String>,
    pub bonus_routes: Vec<BonusRoute>,
    pub map_triggers: MapTriggers,
    pub spawn: Vec3,
    pub spawn_yaw: f32,
    pub brushes: Vec<Brush>,
    pub surfaces: Vec<Surface>,
    pub textures: Vec<CourseTexture>,
    pub visual_meshes: Vec<VisualMesh>,
    pub visual_visibility: Option<crate::visual_visibility::VisualVisibility>,
    pub practice_spawns: Vec<PracticeSpawn>,
    pub sky: Option<[usize; 6]>,
    pub sky_occluders: Vec<VisualOccluder>,
    pub fog: Option<CourseFog>,
    pub zones: Vec<Zone>,
    pub kill_y: f32,
}

#[derive(Clone, Copy)]
pub struct Plane {
    pub normal: Vec3,
    pub distance: f32,
}

/// The interior is the intersection of normal.dot(point) <= distance.
#[derive(Clone)]
pub struct Brush {
    pub planes: BrushPlanes,
    pub min: Vec3,
    pub max: Vec3,
}

pub struct Surface {
    pub vertices: Vec<Vec3>,
    pub color: [f32; 3],
    pub texture: Option<SurfaceTexture>,
    pub lightmap: Option<SurfaceLightmap>,
}

pub struct SurfaceTexture {
    pub index: usize,
    pub uvs: Vec<[f32; 2]>,
}

pub struct SurfaceLightmap {
    pub index: usize,
    pub uvs: Vec<[f32; 2]>,
}

pub struct CourseTexture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub bc: Option<crate::bc_texture::BcTexture>,
    pub alpha: TextureAlpha,
}

pub struct VisualMesh {
    pub vertices: Vec<VisualVertex>,
    pub indices: Vec<u32>,
    pub material: VisualMaterial,
    /// Empty for geometry already in world coordinates.
    pub instances: Vec<VisualInstance>,
    pub clusters: Option<Arc<[usize]>>,
    pub sky: bool,
    pub opacity: Option<crate::visual_opacity::VisualOpacity>,
}

pub struct VisualOccluder {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
}

pub struct VisualInstance {
    pub transform: Mat4,
    /// Starting texel in the baked vertex lighting atlas, if present.
    pub light_offset: Option<u32>,
    pub environment: Option<[usize; 6]>,
    pub lod: Option<VisualLod>,
    pub clusters: Option<Arc<[usize]>>,
    pub sky: bool,
    pub fade: Option<VisualPropFade>,
}

#[derive(Clone)]
pub struct VisualPropFade {
    pub range: [f32; 2],
    pub sky_group: Option<Arc<[(Vec3, f32)]>>,
}

pub struct VisualLod {
    pub prop: usize,
    pub level: usize,
    pub switch_points: std::sync::Arc<[f32]>,
    pub depth_scale: f32,
}

pub struct VisualMaterial {
    pub base: Option<usize>,
    pub second: Option<usize>,
    pub detail: Option<usize>,
    pub normal: Option<usize>,
    pub env_mask: Option<usize>,
    pub environment: Option<[usize; 6]>,
    pub lightmap: Option<usize>,
    pub rgb_exp_lightmap: bool,
    pub prop_light: Option<usize>,
    pub tint: [f32; 3],
    pub opacity: f32,
    pub alpha_cut: Option<f32>,
    pub vertex_alpha: bool,
    pub translucent: bool,
    pub double_sided: bool,
    pub additive: bool,
    pub no_fog: bool,
    pub unlit: bool,
    pub detail_scale: [f32; 2],
    pub detail_factor: f32,
    pub seamless: f32,
    pub sky_camera: Option<(Vec3, f32)>,
    pub env_tint: [f32; 3],
    pub env_shaping: Option<[f32; 3]>,
    pub normal_alpha_mask: bool,
    pub apply_normal: bool,
    pub reflection_only: bool,
    pub lighting_response: Option<[f32; 2]>,
    pub preserve_shadow_color: bool,
}

pub struct VisualVertex {
    pub position: Vec3,
    pub normal: Vec3,
    pub uv: [f32; 2],
    pub light_uv: [f32; 2],
    pub blend: f32,
    pub light_index: f32,
}

pub struct CourseFog {
    pub color: [f32; 3],
    pub start: f32,
    pub end: f32,
    pub max_density: f32,
}

#[derive(Clone, Copy)]
pub enum TextureAlpha {
    Opaque,
    Cutout,
    Blend,
}

#[derive(Clone)]
pub struct PracticeSpawn {
    pub name: String,
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub source_yaw_degrees: Option<f64>,
}

pub struct BonusRoute {
    pub number: u16,
    pub name: String,
    pub spawn: PracticeSpawn,
    pub zones: Vec<Zone>,
    pub revision: String,
}

#[derive(Clone)]
pub struct Zone {
    pub brush: Brush,
    pub kind: ZoneKind,
}

#[derive(Clone)]
pub enum ZoneKind {
    Start,
    StageStart(u16),
    Finish,
    Checkpoint(u16),
    Reset,
    Teleport { position: Vec3, yaw: Option<f32> },
    StageTeleport { position: Vec3, yaw: Option<f32> },
    AuthoredTeleport(Box<AuthoredTeleport>),
    Push { index: usize, velocity: Vec3 },
    NoJump(f32),
    PlayerOutput(Box<PlayerOutputZone>),
}

#[derive(Default)]
pub struct MapTriggers {
    pub spawn_pitch: Option<f32>,
    pub(crate) spawn_source_yaw: Option<f64>,
    pub(crate) brush_index: OnceLock<Option<Arc<BrushIndex>>>,
    pub continuous_stages: bool,
    pub boosted_start_stages: Vec<u16>,
    pub push_after_movement: bool,
    pub source_push_ground_fix: bool,
    pub output_velocity_ground_fix: bool,
    pub stage_handoff_releases_duck: bool,
    pub(crate) vphysics_feature_fix: bool,
    pub(crate) vphysics_contact_format: u8,
    pub(crate) bsp_corner_fix: bool,
    pub(crate) surfd_rules: bool,
    pub telehop_fix: bool,
    pub conveyors: Vec<Conveyor>,
    pub surface_friction: Vec<BrushFriction>,
    pub(crate) recorded_brushes: Vec<RecordedBrush>,
    pub(crate) authored_stage_starts: Vec<Vec<Brush>>,
    pub(crate) authored_stage_spawns: Vec<PracticeSpawn>,
    pub(crate) source_version: Option<String>,
    pub(crate) bonus_pack_sha256: Option<String>,
    pub(crate) bonus_rules: HashMap<u16, BonusRules>,
    pub(crate) movement_volumes: MovementVolumes,
}

#[derive(Default)]
pub(crate) struct BonusRules {
    pub effects: Option<Vec<Zone>>,
    pub push_filters: HashMap<usize, TriggerPredicate>,
    pub geometry: Option<BonusGeometryPatch>,
    pub boosted_start: bool,
    pub telehop_fix: bool,
    pub impulse_limit: bool,
    pub fall_y: Option<f32>,
    pub movement_volumes: MovementVolumes,
}

#[derive(Default)]
pub(crate) struct MovementVolumes {
    pub ladder_ids: std::collections::HashSet<String>,
    pub water_hulls: Vec<AuthoredZoneGeometry>,
}

#[derive(Default)]
pub(crate) struct BonusGeometryPatch {
    pub removed: Vec<usize>,
    pub extra: Vec<(Brush, RecordedBrush, BrushFriction)>,
    cache: Mutex<Weak<BonusCollision>>,
}

pub(crate) struct BonusCollision {
    pub brushes: Vec<Brush>,
    pub recorded_brushes: Vec<RecordedBrush>,
    pub friction: Vec<BrushFriction>,
    pub conveyors: Vec<Conveyor>,
    pub index: Option<Arc<BrushIndex>>,
}

#[derive(Clone)]
pub(crate) struct RecordedBrush {
    pub id: Option<String>,
    pub prop: bool,
    pub axis_aligned_box: bool,
    pub planes: Vec<PrecisePlane>,
    pub leaf_bounds: Vec<(Vec3, Vec3)>,
}

/// Keep dot-product summation in the original Source axis order.
pub(crate) fn source_dot(a: DVec3, b: DVec3) -> f64 {
    a.x * b.x + a.z * b.z + a.y * b.y
}

#[derive(Clone)]
pub struct BrushFriction {
    pub brush: f32,
    pub planes: Vec<Option<f32>>,
}

pub struct Conveyor {
    pub brush_indices: Vec<usize>,
    pub velocity: Vec3,
}

#[derive(Clone)]
pub struct TriggerFilter {
    pub targetname: String,
    pub negated: bool,
}

impl TriggerFilter {
    pub fn accepts(&self, current: &str) -> bool {
        let matches = if let Some(prefix) = self.targetname.strip_suffix('*') {
            current.to_lowercase().starts_with(&prefix.to_lowercase())
        } else {
            current == self.targetname
        };
        matches != self.negated
    }
}

#[derive(Clone)]
pub enum TriggerPredicate {
    Name(TriggerFilter),
    Group {
        all: bool,
        negated: bool,
        filters: Vec<TriggerPredicate>,
    },
}

impl TriggerPredicate {
    pub fn accepts(&self, targetname: &str) -> bool {
        match self {
            Self::Name(filter) => filter.accepts(targetname),
            Self::Group {
                all,
                negated,
                filters,
            } => {
                let accepts = if *all {
                    filters.iter().all(|filter| filter.accepts(targetname))
                } else {
                    filters.iter().any(|filter| filter.accepts(targetname))
                };
                accepts != *negated
            }
        }
    }
}

#[derive(Clone)]
pub struct AuthoredTeleport {
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub source_yaw_degrees: Option<f64>,
    pub landmark: Option<Vec3>,
    pub relative: bool,
    pub keep_view: bool,
    pub stage: Option<u16>,
    pub stage_velocity: bool,
    pub preserve_velocity: bool,
    pub checkpoint: bool,
    pub restart: bool,
    pub filter: Option<TriggerFilter>,
    pub filter_multi: Option<TriggerPredicate>,
    pub carry_push: bool,
}

#[derive(Clone)]
pub struct PlayerOutputZone {
    pub index: usize,
    pub filter: Option<TriggerFilter>,
    pub filter_multi: Option<TriggerPredicate>,
    pub on_start: Vec<PlayerOutput>,
    pub on_end: Vec<PlayerOutput>,
    pub on_touch: Vec<PlayerOutput>,
}

#[derive(Clone)]
pub struct PlayerOutput {
    pub delay_ticks: u32,
    pub value: PlayerOutputValue,
}

#[derive(Clone)]
pub enum PlayerOutputValue {
    TargetName(String),
    Velocity(Vec3),
    BaseVelocity(Vec3),
    Gravity(f32),
    LaggedMovement { scale: f32, suppress_jump: bool },
}

impl Brush {
    pub fn box_at(min: Vec3, max: Vec3) -> Self {
        let planes = [Vec3::X, Vec3::Y, Vec3::Z]
            .into_iter()
            .flat_map(|normal| {
                [
                    Plane {
                        normal,
                        distance: normal.dot(max),
                    },
                    Plane {
                        normal: -normal,
                        distance: -normal.dot(min),
                    },
                ]
            })
            .collect();
        Self { planes, min, max }
    }
}

impl Course {
    pub(crate) fn collision_index(&self) -> Option<Arc<BrushIndex>> {
        let build = || BrushIndex::new(&self.brushes).map(Arc::new);
        match self.map_triggers.brush_index.get_or_init(build) {
            Some(index) if index.covers_bounds(&self.brushes) => Some(Arc::clone(index)),
            _ => build(),
        }
    }

    pub fn bonus(&self, number: u16) -> Option<&BonusRoute> {
        self.bonus_routes
            .iter()
            .find(|bonus| bonus.number == number)
    }

    pub(crate) fn bonus_rules(&self, bonus: Option<u16>) -> Option<&BonusRules> {
        self.map_triggers.bonus_rules.get(&bonus?)
    }

    pub(crate) fn bonus_collision(&self, bonus: Option<u16>) -> Option<Arc<BonusCollision>> {
        let patch = self.bonus_rules(bonus)?.geometry.as_ref()?;
        let mut cache = patch
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = cache.upgrade() {
            return Some(cached);
        }
        let base = &self.map_triggers;
        let solid_count = base.recorded_brushes.len();
        let mut brushes =
            Vec::with_capacity(self.brushes.len() - patch.removed.len() + patch.extra.len());
        let mut recorded_brushes = Vec::with_capacity(solid_count + patch.extra.len());
        let mut friction = Vec::with_capacity(recorded_brushes.capacity());
        let mut remap = vec![None; self.brushes.len()];
        for (index, brush) in self.brushes[..solid_count].iter().enumerate() {
            if patch.removed.binary_search(&index).is_ok() {
                continue;
            }
            remap[index] = Some(brushes.len());
            brushes.push(brush.clone());
            recorded_brushes.push(base.recorded_brushes[index].clone());
            friction.push(base.surface_friction[index].clone());
        }
        for (brush, recorded, surface) in &patch.extra {
            brushes.push(brush.clone());
            recorded_brushes.push(recorded.clone());
            friction.push(surface.clone());
        }
        for (index, brush) in self.brushes.iter().enumerate().skip(solid_count) {
            if patch.removed.binary_search(&index).is_ok() {
                continue;
            }
            remap[index] = Some(brushes.len());
            brushes.push(brush.clone());
        }
        let conveyors = base
            .conveyors
            .iter()
            .filter_map(|conveyor| {
                let brush_indices: Vec<_> = conveyor
                    .brush_indices
                    .iter()
                    .filter_map(|&index| remap.get(index).copied().flatten())
                    .collect();
                (!brush_indices.is_empty()).then_some(Conveyor {
                    brush_indices,
                    velocity: conveyor.velocity,
                })
            })
            .collect();
        let index = BrushIndex::new(&brushes).map(Arc::new);
        let collision = Arc::new(BonusCollision {
            brushes,
            recorded_brushes,
            friction,
            conveyors,
            index,
        });
        *cache = Arc::downgrade(&collision);
        Some(collision)
    }

    /// Board identity includes the verified parent geometry and bonus definition.
    pub fn board_revision(&self, bonus: Option<u16>) -> Option<String> {
        self.board_revision_for_profile(bonus, self.physics_profile_for(bonus))
    }

    pub fn supports_board_replay(&self, bonus: Option<u16>, revision: &str) -> bool {
        let (_, profile) = PhysicsProfile::split_revision(revision);
        self.supports_physics_profile(profile, bonus)
            && self.board_revision_for_profile(bonus, profile).as_deref() == Some(revision)
    }

    fn board_revision_for_profile(
        &self,
        bonus: Option<u16>,
        profile: PhysicsProfile,
    ) -> Option<String> {
        let revision = self.catalog_revision.as_ref()?;
        let revision = match bonus {
            Some(number) => format!("{revision}:bonus:{number}:{}", self.bonus(number)?.revision),
            None => revision.clone(),
        };
        Some(profile.revision(revision))
    }

    pub(crate) fn replay_revision(&self, bonus: Option<u16>) -> Option<String> {
        self.replay_revision_for_profile(bonus, self.physics_profile_for(bonus))
    }

    pub(crate) fn physics_profile_for(&self, bonus: Option<u16>) -> PhysicsProfile {
        if self.map_triggers.surfd_rules {
            if self.bonus_rules(bonus).is_some() {
                PhysicsProfile::SurfdBonus
            } else {
                PhysicsProfile::Surfd
            }
        } else if self.map_triggers.vphysics_feature_fix {
            PhysicsProfile::AuthoredContacts
        } else {
            PhysicsProfile::Legacy
        }
    }

    fn supports_physics_profile(&self, profile: PhysicsProfile, bonus: Option<u16>) -> bool {
        match profile {
            PhysicsProfile::Legacy => true,
            PhysicsProfile::AuthoredContacts => self.map_triggers.vphysics_feature_fix,
            PhysicsProfile::Surfd => self.map_triggers.surfd_rules,
            PhysicsProfile::SurfdBonus => {
                self.map_triggers.surfd_rules && self.bonus_rules(bonus).is_some()
            }
        }
    }

    pub(crate) fn replay_physics_profile(
        &self,
        bonus: Option<u16>,
        revision: &str,
    ) -> Option<PhysicsProfile> {
        let (_, profile) = PhysicsProfile::split_revision(revision);
        (self.supports_physics_profile(profile, bonus)
            && self.replay_revision_for_profile(bonus, profile).as_deref() == Some(revision))
        .then_some(profile)
    }

    fn replay_revision_for_profile(
        &self,
        bonus: Option<u16>,
        profile: PhysicsProfile,
    ) -> Option<String> {
        if self.catalog_revision.is_some() {
            return self.board_revision_for_profile(bonus, profile);
        }
        let (spawn, yaw, zones) = match bonus {
            Some(number) => {
                let bonus = self.bonus(number)?;
                (
                    bonus.spawn.position,
                    bonus.spawn.yaw,
                    bonus.zones.as_slice(),
                )
            }
            None => (self.spawn, self.spawn_yaw, self.zones.as_slice()),
        };
        let mut digest = Sha256::new();
        digest.update(b"surf-local-course-v2");
        if profile == PhysicsProfile::SurfdBonus
            && let Some(hash) = &self.map_triggers.bonus_pack_sha256
        {
            digest.update(hash.as_bytes());
        }
        hash_vec(&mut digest, spawn);
        digest.update(yaw.to_le_bytes());
        digest.update(self.kill_y.to_le_bytes());
        let triggers = &self.map_triggers;
        digest.update([
            u8::from(triggers.continuous_stages),
            u8::from(triggers.push_after_movement),
            u8::from(triggers.source_push_ground_fix),
            u8::from(triggers.output_velocity_ground_fix),
            u8::from(triggers.stage_handoff_releases_duck),
            u8::from(triggers.telehop_fix),
        ]);
        digest.update((triggers.boosted_start_stages.len() as u64).to_le_bytes());
        for stage in &triggers.boosted_start_stages {
            digest.update(stage.to_le_bytes());
        }
        digest.update((triggers.conveyors.len() as u64).to_le_bytes());
        for conveyor in &triggers.conveyors {
            hash_vec(&mut digest, conveyor.velocity);
            digest.update((conveyor.brush_indices.len() as u64).to_le_bytes());
            for index in &conveyor.brush_indices {
                digest.update((*index as u64).to_le_bytes());
            }
        }
        digest.update((triggers.surface_friction.len() as u64).to_le_bytes());
        for friction in &triggers.surface_friction {
            digest.update(friction.brush.to_le_bytes());
            digest.update((friction.planes.len() as u64).to_le_bytes());
            for plane in &friction.planes {
                digest.update(plane.unwrap_or(friction.brush).to_le_bytes());
            }
        }
        digest.update((self.brushes.len() as u64).to_le_bytes());
        for brush in &self.brushes {
            hash_brush(&mut digest, brush);
        }
        digest.update((zones.len() as u64).to_le_bytes());
        let inherited = bonus.into_iter().flat_map(|_| &self.zones).filter(|zone| {
            matches!(
                zone.kind,
                ZoneKind::Push { .. } | ZoneKind::NoJump(_) | ZoneKind::PlayerOutput(_)
            )
        });
        for zone in zones.iter().chain(inherited) {
            hash_brush(&mut digest, &zone.brush);
            if profile != PhysicsProfile::Legacy
                && let Some(geometry) = zone.brush.planes.zone_geometry()
            {
                digest.update((geometry.planes.len() as u64).to_le_bytes());
                for vector in [geometry.min, geometry.max] {
                    for value in vector.to_array() {
                        digest.update(value.to_le_bytes());
                    }
                }
                for &(normal, distance) in &geometry.planes {
                    for value in normal.to_array() {
                        digest.update(value.to_le_bytes());
                    }
                    digest.update(distance.to_le_bytes());
                }
            }
            match zone.kind {
                ZoneKind::Start => digest.update([0]),
                ZoneKind::Finish => digest.update([1]),
                ZoneKind::Reset => digest.update([2]),
                ZoneKind::Checkpoint(order) | ZoneKind::StageStart(order) => {
                    digest.update([if matches!(zone.kind, ZoneKind::Checkpoint(_)) {
                        3
                    } else {
                        4
                    }]);
                    digest.update(order.to_le_bytes());
                }
                ZoneKind::Teleport { position, yaw }
                | ZoneKind::StageTeleport { position, yaw } => {
                    digest.update([if matches!(zone.kind, ZoneKind::Teleport { .. }) {
                        5
                    } else {
                        6
                    }]);
                    hash_vec(&mut digest, position);
                    digest.update([u8::from(yaw.is_some())]);
                    if let Some(yaw) = yaw {
                        digest.update(yaw.to_le_bytes());
                    }
                }
                ZoneKind::AuthoredTeleport(ref teleport) => {
                    digest.update([
                        7,
                        u8::from(teleport.relative),
                        u8::from(teleport.keep_view),
                        u8::from(teleport.stage_velocity),
                        u8::from(teleport.preserve_velocity),
                        u8::from(teleport.checkpoint),
                        u8::from(teleport.restart),
                        u8::from(teleport.landmark.is_some()),
                    ]);
                    hash_vec(&mut digest, teleport.position);
                    if let Some(landmark) = teleport.landmark {
                        hash_vec(&mut digest, landmark);
                    }
                    digest.update(teleport.yaw.to_le_bytes());
                    digest.update(teleport.pitch.to_le_bytes());
                    digest.update(teleport.stage.unwrap_or(0).to_le_bytes());
                    hash_filter(&mut digest, teleport.filter.as_ref());
                }
                ZoneKind::Push { index, velocity } => {
                    digest.update([8]);
                    digest.update((index as u64).to_le_bytes());
                    hash_vec(&mut digest, velocity);
                }
                ZoneKind::NoJump(scale) => {
                    digest.update([9]);
                    digest.update(scale.to_le_bytes());
                }
                ZoneKind::PlayerOutput(ref output) => {
                    digest.update([10]);
                    digest.update((output.index as u64).to_le_bytes());
                    hash_filter(&mut digest, output.filter.as_ref());
                    for actions in [&output.on_start, &output.on_end] {
                        digest.update((actions.len() as u64).to_le_bytes());
                        for action in actions {
                            digest.update(action.delay_ticks.to_le_bytes());
                            match &action.value {
                                PlayerOutputValue::TargetName(name) => {
                                    digest.update([0]);
                                    digest.update((name.len() as u64).to_le_bytes());
                                    digest.update(name.as_bytes());
                                }
                                PlayerOutputValue::Velocity(value) => {
                                    digest.update([1]);
                                    hash_vec(&mut digest, *value);
                                }
                                PlayerOutputValue::BaseVelocity(value) => {
                                    digest.update([2]);
                                    hash_vec(&mut digest, *value);
                                }
                                PlayerOutputValue::Gravity(value) => {
                                    digest.update([3]);
                                    digest.update(value.to_le_bytes());
                                }
                                PlayerOutputValue::LaggedMovement {
                                    scale,
                                    suppress_jump,
                                } => {
                                    digest.update([4, u8::from(*suppress_jump)]);
                                    digest.update(scale.to_le_bytes());
                                }
                            }
                        }
                    }
                }
            }
        }
        Some(profile.revision(format!("local:{:x}", digest.finalize())))
    }

    pub fn practice() -> Self {
        let mut course = Self {
            name: "Downline".to_owned(),
            catalog_revision: Some("builtin-downline-v1".to_owned()),
            bonus_routes: Vec::new(),
            map_triggers: MapTriggers::default(),
            spawn: Vec3::new(72.0, 520.05, 25.0),
            spawn_yaw: 0.0,
            brushes: Vec::new(),
            surfaces: Vec::new(),
            textures: Vec::new(),
            visual_meshes: Vec::new(),
            visual_visibility: None,
            practice_spawns: vec![PracticeSpawn {
                name: "Finish deck".to_owned(),
                position: Vec3::new(0.0, -492.95, -2200.0),
                yaw: 0.0,
                pitch: -0.19,
                source_yaw_degrees: None,
            }],
            sky: None,
            sky_occluders: Vec::new(),
            fog: None,
            zones: Vec::new(),
            kill_y: -1200.0,
        };

        let concrete = [0.31, 0.39, 0.43];
        let deck = [0.22, 0.30, 0.34];
        let cyan = [0.40, 0.93, 0.87];
        let gold = [0.99, 0.72, 0.36];
        course.block([-184., 476., -245.], [184., 520., 146.], deck, true);
        course.block([-184., 520.1, -245.], [184., 520.3, -233.], cyan, false);
        for x in [-184., 172.] {
            course.block([x, 520.1, -224.], [x + 12., 520.3, 146.], cyan, false);
        }
        for z in [-26., -81., -136., -191.] {
            course.block([68., 520.1, z], [76., 520.3, z + 27.], cyan, false);
        }

        course.ramp(-232., -2070., 437., 23.);

        course.block(
            [-430., -565., -2950.],
            [420., -493., -2100.],
            concrete,
            true,
        );
        course.block([-430., -492.9, -2150.], [420., -492.5, -2100.], gold, false);
        for x in [-430., 404.] {
            course.block([x, -492.9, -2950.], [x + 16., -492.5, -2150.], gold, false);
        }
        for x in [-420., 389.] {
            course.block([x, -493., -2490.], [x + 22., -145., -2440.], gold, false);
        }
        course.block([-420., -167., -2490.], [411., -145., -2440.], gold, false);
        for x in (-420..410).step_by(58) {
            course.block(
                [x as f32, -492.9, -2490.],
                [(x + 29) as f32, -492.5, -2440.],
                gold,
                false,
            );
        }

        course.zone([-185., 518., -194.], [185., 630., 148.], ZoneKind::Start);
        course.zone(
            [-385., -790., -1154.],
            [335., 390., -1142.],
            ZoneKind::Checkpoint(1),
        );
        course.zone(
            [-420., -496., -2490.],
            [410., -145., -2440.],
            ZoneKind::Finish,
        );
        course
    }

    pub(crate) fn zone(&mut self, min: [f32; 3], max: [f32; 3], kind: ZoneKind) {
        self.zones.push(Zone {
            brush: Brush::box_at(min.into(), max.into()),
            kind,
        });
    }

    pub(crate) fn block(&mut self, min: [f32; 3], max: [f32; 3], color: [f32; 3], solid: bool) {
        let [x0, y0, z0] = min;
        let [x1, y1, z1] = max;
        let vertices = [
            Vec3::new(x0, y0, z0),
            Vec3::new(x1, y0, z0),
            Vec3::new(x1, y1, z0),
            Vec3::new(x0, y1, z0),
            Vec3::new(x0, y0, z1),
            Vec3::new(x1, y0, z1),
            Vec3::new(x1, y1, z1),
            Vec3::new(x0, y1, z1),
        ];
        self.polyhedron(
            &vertices,
            &[
                &[0, 3, 2, 1],
                &[4, 5, 6, 7],
                &[0, 4, 7, 3],
                &[1, 2, 6, 5],
                &[0, 1, 5, 4],
                &[3, 7, 6, 2],
            ],
            color,
            solid,
        );
    }

    fn ramp(&mut self, front: f32, back: f32, high: f32, low: f32) {
        let profile = |height, z| {
            [
                Vec3::new(-335., height - 440., z),
                Vec3::new(-38., height, z),
                Vec3::new(259., height - 440., z),
            ]
        };
        let [left, peak, right] = profile(high, front);
        let [far_left, far_peak, far_right] = profile(low, back);
        self.polyhedron(
            &[left, peak, right, far_left, far_peak, far_right],
            &[
                &[0, 1, 2],
                &[3, 5, 4],
                &[0, 3, 4, 1],
                &[1, 4, 5, 2],
                &[0, 2, 5, 3],
            ],
            [0.24, 0.51, 0.54],
            true,
        );

        for (near_edge, far_edge) in [(left, far_left), (right, far_right)] {
            let point = |along: f32, across: f32| {
                peak.lerp(far_peak, along)
                    .lerp(near_edge.lerp(far_edge, along), across)
                    + Vec3::Y * 0.6
            };
            let color = [0.49, 0.94, 0.87];
            for (a, b) in [(0.017, 0.030), (0.94, 0.955)] {
                self.surfaces.push(Surface {
                    vertices: vec![point(0., a), point(0., b), point(1., b), point(1., a)],
                    color,
                    texture: None,
                    lightmap: None,
                });
            }
            for index in 1..10 {
                let t = index as f32 / 10.0;
                self.surfaces.push(Surface {
                    vertices: vec![
                        point(t, 0.03),
                        point(t, 0.94),
                        point(t + 0.0017, 0.94),
                        point(t + 0.0017, 0.03),
                    ],
                    color: [0.39, 0.69, 0.69],
                    texture: None,
                    lightmap: None,
                });
            }
        }
    }

    pub(crate) fn polyhedron(
        &mut self,
        vertices: &[Vec3],
        faces: &[&[usize]],
        color: [f32; 3],
        solid: bool,
    ) {
        let center = vertices.iter().sum::<Vec3>() / vertices.len() as f32;
        let min = vertices
            .iter()
            .copied()
            .fold(Vec3::splat(f32::INFINITY), Vec3::min);
        let max = vertices
            .iter()
            .copied()
            .fold(Vec3::splat(f32::NEG_INFINITY), Vec3::max);
        let mut brush = Brush::box_at(min, max);
        for face in faces {
            let mut points: Vec<_> = face.iter().map(|&i| vertices[i]).collect();
            let mut normal = (points[1] - points[0])
                .cross(points[2] - points[0])
                .normalize();
            if normal.dot(points[0] - center) < 0.0 {
                normal = -normal;
                points.reverse();
            }
            if solid
                && !brush
                    .planes
                    .iter()
                    .any(|plane| plane.normal.dot(normal) > 0.99999)
            {
                brush.planes.push(Plane {
                    normal,
                    distance: normal.dot(points[0]),
                });
            }
            self.surfaces.push(Surface {
                vertices: points,
                color,
                texture: None,
                lightmap: None,
            });
        }
        if solid {
            self.brushes.push(brush);
        }
    }
}
