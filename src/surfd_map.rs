use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
    sync::Arc,
};

use bevy_math::{DVec3, Vec3};
use flate2::read::MultiGzDecoder;
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::{
    course::{
        AuthoredTeleport, AuthoredZoneGeometry, BonusGeometryPatch, BonusRoute, BonusRules, Brush,
        BrushFriction, Conveyor, Course, DisplacementHull, MapTriggers, MovementVolumes, Plane,
        PlayerOutput, PlayerOutputValue, PlayerOutputZone, PracticeSpawn, RecordedBrush, Surface,
        TriggerFilter, TriggerPredicate, Zone, ZoneKind,
    },
    movement::TICK_HZ,
};

#[path = "surfd_visual.rs"]
mod visual;
pub use visual::{visual_dependencies, visual_dependencies_for_gpu};

const MAX_COLLISION_JSON: usize = 192 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct VisualLoadOptions {
    pub use_bc: bool,
    pub parallel_packs: bool,
}

impl Default for VisualLoadOptions {
    fn default() -> Self {
        Self {
            use_bc: false,
            parallel_packs: true,
        }
    }
}

/// Load a Surfd collision export and an optional visual manifest. The selected
/// manifest's `.visual-assets.gz` files must be next to the manifest itself.
pub fn load(geometry: &Path, visuals: Option<&Path>) -> Result<Course, String> {
    let data = read_json(geometry, MAX_COLLISION_JSON)?;
    build_course(data, visuals, VisualLoadOptions::default(), |_| Ok(()))
}

pub fn load_verified(
    geometry: &Path,
    visuals: Option<&Path>,
    revision: &str,
) -> Result<Course, String> {
    load_verified_for_gpu(geometry, visuals, revision, false)
}

pub fn load_verified_for_gpu(
    geometry: &Path,
    visuals: Option<&Path>,
    revision: &str,
    use_bc: bool,
) -> Result<Course, String> {
    load_verified_with_visual_options(
        geometry,
        visuals,
        revision,
        VisualLoadOptions {
            use_bc,
            ..VisualLoadOptions::default()
        },
    )
}

pub fn load_verified_with_visual_options(
    geometry: &Path,
    visuals: Option<&Path>,
    revision: &str,
    options: VisualLoadOptions,
) -> Result<Course, String> {
    load_verified_with_visual_packs(geometry, visuals, revision, options, |_| Ok(()))
}

/// Prepare the required pack files beside the manifest before reading them.
/// The same validated manifest determines dependencies and decodes the course.
pub fn load_verified_with_visual_packs(
    geometry: &Path,
    visuals: Option<&Path>,
    revision: &str,
    options: VisualLoadOptions,
    prepare_packs: impl FnOnce(&[&str]) -> Result<(), String>,
) -> Result<Course, String> {
    let data = read_revision(geometry, MAX_COLLISION_JSON, Some(revision))?;
    build_course(data, visuals, options, prepare_packs)
}

fn build_course(
    data: Collision,
    visuals: Option<&Path>,
    options: VisualLoadOptions,
    prepare_packs: impl FnOnce(&[&str]) -> Result<(), String>,
) -> Result<Course, String> {
    let mut course = data.to_course(visuals.is_none())?;
    if let Some(manifest) = visuals {
        visual::populate(
            &mut course,
            manifest,
            &data.id,
            data.source.as_ref(),
            options,
            prepare_packs,
        )?;
    }
    Ok(course)
}

pub fn load_collision(geometry: &Path) -> Result<Course, String> {
    read_json::<Collision>(geometry, MAX_COLLISION_JSON)?.to_course(false)
}

pub fn load_collision_verified(geometry: &Path, revision: &str) -> Result<Course, String> {
    read_revision::<Collision>(geometry, MAX_COLLISION_JSON, Some(revision))?.to_course(false)
}

pub fn add_bonus_spawns(course: &mut Course, pack: &Path) -> Result<(), String> {
    load_bonus_pack(course, pack, None)
}

pub fn add_bonus_spawns_verified(
    course: &mut Course,
    pack: &Path,
    revision: &str,
) -> Result<(), String> {
    load_bonus_pack(course, pack, Some(revision))
}

fn load_bonus_pack(course: &mut Course, path: &Path, expected: Option<&str>) -> Result<(), String> {
    let bytes = read_asset(path, MAX_COLLISION_JSON)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    if expected.is_some_and(|expected| !hash.eq_ignore_ascii_case(expected)) {
        return Err(format!("{}: map asset checksum mismatch", path.display()));
    }
    let pack =
        serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    add_bonuses(course, pack, hash)
}

fn add_bonuses(course: &mut Course, pack: BonusPack, pack_sha256: String) -> Result<(), String> {
    if pack.map_id != course.name {
        return Err(format!(
            "Bonus pack is for {}, not {}",
            pack.map_id, course.name
        ));
    }
    if let (Some(actual), Some(expected)) = (&pack.geometry_sha256, &course.catalog_revision)
        && actual != expected
    {
        return Err("Bonus pack is for a different geometry revision".into());
    }
    if let (Some(actual), Some(expected)) = (&pack.map_version, &course.map_triggers.source_version)
        && actual != expected
    {
        return Err("Bonus pack belongs to a different map version".into());
    }
    let mut routes = Vec::new();
    let mut rules = HashMap::new();
    let mut spawns = Vec::new();
    for bonus in pack.bonuses.into_iter().filter(|bonus| bonus.number > 0) {
        let name = bonus
            .name
            .as_ref()
            .filter(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("Bonus {}", bonus.number));
        let spawn = bonus.spawn.practice(name.clone())?;
        let mut zones = Vec::new();
        for zone in &bonus.zones {
            zone.add(&mut zones)?;
        }
        for teleport in &bonus.teleport_zones {
            teleport.add(teleport.destination, &mut zones, true)?;
        }
        spawns.push(spawn.clone());
        if zones
            .iter()
            .any(|zone| matches!(zone.kind, ZoneKind::Start))
            && zones
                .iter()
                .any(|zone| matches!(zone.kind, ZoneKind::Finish))
        {
            let revision = bonus
                .revision
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| bonus.revision.to_string());
            if rules.insert(bonus.number, bonus.rules(course)?).is_some() {
                return Err("Duplicate number in the bonus pack".into());
            }
            routes.push(BonusRoute {
                number: bonus.number,
                name,
                spawn,
                zones,
                revision,
            });
        }
    }
    course.practice_spawns.extend(spawns);
    course.bonus_routes = routes;
    course.map_triggers.bonus_rules = rules;
    course.map_triggers.bonus_pack_sha256 = Some(pack_sha256);
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BonusPack {
    map_id: String,
    map_version: Option<String>,
    geometry_sha256: Option<String>,
    bonuses: Vec<Bonus>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bonus {
    number: u16,
    name: Option<String>,
    spawn: Spawn,
    revision: serde_json::Value,
    #[serde(default)]
    zones: Vec<RawZone>,
    #[serde(default)]
    teleport_zones: Vec<Teleport>,
    #[serde(default)]
    push_zones: Vec<RawPush>,
    #[serde(default)]
    no_jump_zones: Vec<RawNoJump>,
    #[serde(default)]
    player_output_zones: Vec<RawOutputZone>,
    #[serde(default)]
    boosted_start: bool,
    #[serde(default)]
    telehop_fix: bool,
    #[serde(default)]
    impulse_limit: bool,
    fall_z: Option<f32>,
    #[serde(default)]
    extra_brushes: Vec<RawBrush>,
    #[serde(default)]
    removed_brush_ids: Vec<String>,
    #[serde(default)]
    removed_displacement_ids: Vec<u32>,
    #[serde(default)]
    ladder_brush_ids: Vec<String>,
    #[serde(default)]
    water_zones: Vec<RawWaterZone>,
}

impl Bonus {
    fn rules(&self, course: &Course) -> Result<BonusRules, String> {
        let mut rules = BonusRules {
            boosted_start: self.boosted_start,
            telehop_fix: self.telehop_fix,
            impulse_limit: self.impulse_limit,
            fall_y: self.fall_z,
            geometry: self.geometry_patch(course)?,
            movement_volumes: movement_volumes(&self.ladder_brush_ids, &self.water_zones)?,
            ..Default::default()
        };
        if self.push_zones.is_empty()
            && self.no_jump_zones.is_empty()
            && self.player_output_zones.is_empty()
        {
            return Ok(rules);
        }
        let mut effects: Vec<_> = course
            .zones
            .iter()
            .filter(|zone| {
                matches!(
                    zone.kind,
                    ZoneKind::Push { .. } | ZoneKind::NoJump(_) | ZoneKind::PlayerOutput(_)
                )
            })
            .cloned()
            .collect();
        let push_start = effects
            .iter()
            .filter_map(|zone| match zone.kind {
                ZoneKind::Push { index, .. } => Some(index + 1),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let output_start = effects
            .iter()
            .filter_map(|zone| match &zone.kind {
                ZoneKind::PlayerOutput(output) => Some(output.index + 1),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        for (offset, push) in self.push_zones.iter().enumerate() {
            let index = push_start + offset;
            push.add(index, &mut effects, true)?;
            if let Some(filter) = push.predicate() {
                rules.push_filters.insert(index, filter);
            }
        }
        for no_jump in &self.no_jump_zones {
            no_jump.add(&mut effects)?;
        }
        for (offset, output) in self.player_output_zones.iter().enumerate() {
            output.add(output_start + offset, &mut effects, true)?;
        }
        rules.effects = Some(effects);
        Ok(rules)
    }

    fn geometry_patch(&self, course: &Course) -> Result<Option<BonusGeometryPatch>, String> {
        if self.extra_brushes.is_empty()
            && self.removed_brush_ids.is_empty()
            && self.removed_displacement_ids.is_empty()
        {
            return Ok(None);
        }
        let mut patch = BonusGeometryPatch::default();
        let base = &course.map_triggers;
        let ids: HashMap<_, _> = base
            .recorded_brushes
            .iter()
            .enumerate()
            .filter_map(|(index, brush)| brush.id.as_deref().map(|id| (id, index)))
            .collect();
        for id in &self.removed_brush_ids {
            patch.removed.push(
                *ids.get(id.as_str())
                    .ok_or_else(|| format!("Bonus removes unknown brush {id}"))?,
            );
        }
        let mut remaining: HashSet<_> = self.removed_displacement_ids.iter().copied().collect();
        for (index, brush) in course
            .brushes
            .iter()
            .enumerate()
            .skip(base.recorded_brushes.len())
        {
            if let Some((surface, _)) = brush.planes.displacement_id()
                && self.removed_displacement_ids.contains(&surface)
            {
                remaining.remove(&surface);
                patch.removed.push(index);
            }
        }
        if !remaining.is_empty() {
            return Err("Bonus removes an unknown displacement".into());
        }
        patch.removed.sort_unstable();
        patch.removed.dedup();
        let mut added = HashSet::new();
        for raw in &self.extra_brushes {
            let id = raw.id.as_deref().ok_or("Bonus brush is missing its ID")?;
            if ids.contains_key(id) || !added.insert(id) {
                return Err(format!("Duplicate bonus brush {id}"));
            }
            patch.extra.push((
                raw.convert()?,
                raw.recorded(base.bsp_corner_fix),
                raw.friction(),
            ));
        }
        Ok(Some(patch))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Collision {
    id: String,
    version: Option<String>,
    spawn: Spawn,
    brushes: Vec<RawBrush>,
    #[serde(default)]
    displacement_triangles: Vec<DisplacementTriangle>,
    #[serde(default)]
    zones: Vec<RawZone>,
    #[serde(default)]
    teleport_zones: Vec<Teleport>,
    #[serde(default)]
    stages: Vec<Stage>,
    stage_progression: Option<String>,
    #[serde(default)]
    boosted_start_stages: Vec<u16>,
    push_timing: Option<String>,
    #[serde(default)]
    push_zones: Vec<RawPush>,
    #[serde(default)]
    no_jump_zones: Vec<RawNoJump>,
    #[serde(default)]
    conveyors: Vec<RawConveyor>,
    #[serde(default)]
    player_output_zones: Vec<RawOutputZone>,
    #[serde(default)]
    source_push_ground_fix: bool,
    #[serde(default)]
    output_velocity_ground_fix: bool,
    #[serde(default)]
    stage_handoff_releases_duck: bool,
    #[serde(default)]
    vphysics_feature_fix: bool,
    #[serde(default)]
    bsp_corner_fix: bool,
    #[serde(default)]
    telehop_fix: bool,
    #[serde(default)]
    practice_spawns: Vec<Spawn>,
    #[serde(default)]
    practice_stations: Vec<Station>,
    fall_z: Option<f32>,
    source: Option<Source>,
    #[serde(default)]
    ladder_brush_ids: Vec<String>,
    #[serde(default)]
    water_zones: Vec<RawWaterZone>,
}

#[derive(Deserialize)]
struct RawWaterZone {
    hulls: Vec<RawBrush>,
}

fn movement_volumes(
    ladder_brush_ids: &[String],
    water_zones: &[RawWaterZone],
) -> Result<MovementVolumes, String> {
    let ladder_ids: HashSet<_> = ladder_brush_ids.iter().cloned().collect();
    if ladder_ids.len() != ladder_brush_ids.len() || ladder_ids.contains("") {
        return Err("Surfd ladder brush IDs must be nonempty and unique".into());
    }
    let mut water_hulls = Vec::new();
    for zone in water_zones {
        if zone.hulls.is_empty() {
            return Err("Surfd water zones must have convex hulls".into());
        }
        for hull in &zone.hulls {
            hull.convert()?;
            let a = hull.min.precise_world();
            let b = hull.max.precise_world();
            water_hulls.push(AuthoredZoneGeometry {
                min: a.min(b),
                max: a.max(b),
                planes: hull
                    .planes
                    .iter()
                    .map(|p| (p.normal.precise_world(), p.distance))
                    .collect(),
            });
        }
    }
    Ok(MovementVolumes {
        ladder_ids,
        water_hulls,
    })
}

#[derive(Deserialize)]
struct Source {
    sha1: Option<String>,
}

#[derive(Deserialize, Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
    z: f64,
}

impl Point {
    fn world(self) -> Vec3 {
        self.precise_world().as_vec3()
    }

    fn precise_world(self) -> DVec3 {
        DVec3::new(self.x, self.z, -self.y)
    }
}

#[derive(Deserialize, Clone, Copy)]
struct Spawn {
    position: Point,
    #[serde(default)]
    yaw: f64,
    #[serde(default)]
    pitch: f32,
}

impl Spawn {
    fn yaw(self) -> f32 {
        (self.yaw as f32 - 90.0)
            .to_radians()
            .rem_euclid(std::f32::consts::TAU)
    }

    fn practice(self, name: String) -> Result<PracticeSpawn, String> {
        let position = self.position.world();
        if !position.is_finite() || !(self.yaw as f32).is_finite() || !self.pitch.is_finite() {
            return Err("Invalid Surfd spawn".into());
        }
        Ok(PracticeSpawn {
            name,
            position,
            yaw: self.yaw(),
            pitch: -self.pitch.to_radians(),
            source_yaw_degrees: Some(if self.yaw.abs() <= 360.0 {
                self.yaw
            } else {
                crate::movement::normalize_source_yaw(self.yaw)
            }),
        })
    }
}

#[derive(Deserialize)]
struct Stage {
    spawn: Spawn,
    start: Option<ZoneBounds>,
}

#[derive(Deserialize)]
struct Station {
    name: String,
    spawn: Spawn,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBrush {
    id: Option<String>,
    min: Point,
    max: Point,
    planes: Vec<RawPlane>,
    #[serde(default)]
    vertices: Vec<Point>,
    #[serde(default)]
    faces: Vec<Vec<usize>>,
    color: Option<String>,
    friction: Option<f32>,
    collision_model: Option<String>,
    #[serde(default)]
    leaf_bounds: Vec<ZoneBounds>,
}

#[derive(Deserialize)]
struct RawPlane {
    normal: Point,
    distance: f64,
    friction: Option<f32>,
}

impl RawBrush {
    fn convert(&self) -> Result<Brush, String> {
        let (min, max) = bounds(self.min, self.max);
        if !min.is_finite() || !max.is_finite() || self.planes.is_empty() {
            return Err("Invalid Surfd brush bounds or planes".into());
        }
        let mut planes = Vec::with_capacity(self.planes.len());
        for plane in &self.planes {
            let normal = plane.normal.world();
            let length = normal.length();
            if !length.is_finite() || length < 1e-6 || !plane.distance.is_finite() {
                return Err("Invalid Surfd brush plane".into());
            }
            planes.push(Plane {
                normal: normal / length,
                distance: plane.distance as f32 / length,
            });
        }
        Ok(Brush {
            planes: planes.into(),
            min,
            max,
        })
    }

    fn recorded(&self, bsp_corner_fix: bool) -> RecordedBrush {
        RecordedBrush {
            id: self.id.clone(),
            prop: self.collision_model.is_some(),
            axis_aligned_box: bsp_corner_fix
                && self.planes.len() == 6
                && self.planes.iter().all(|plane| {
                    let axes = plane.normal.precise_world().to_array();
                    axes.into_iter().filter(|axis| *axis != 0.0).count() == 1
                        && axes.into_iter().any(|axis| axis.abs() == 1.0)
                }),
            planes: self
                .planes
                .iter()
                .map(|plane| (plane.normal.precise_world(), plane.distance))
                .collect(),
            leaf_bounds: self
                .leaf_bounds
                .iter()
                .map(|leaf| bounds(leaf.min, leaf.max))
                .collect(),
        }
    }

    fn friction(&self) -> BrushFriction {
        BrushFriction {
            brush: self.friction.unwrap_or(1.0),
            planes: if self.planes.iter().any(|plane| plane.friction.is_some()) {
                self.planes.iter().map(|plane| plane.friction).collect()
            } else {
                Vec::new()
            },
        }
    }

    fn add_surfaces(&self, out: &mut Vec<Surface>) -> Result<(), String> {
        let color = self
            .color
            .as_deref()
            .and_then(hex_color)
            .unwrap_or([0.68, 0.73, 0.77]);
        for face in self.faces.iter().filter(|face| face.len() >= 3) {
            let vertices = face
                .iter()
                .map(|index| {
                    self.vertices
                        .get(*index)
                        .map(|p| p.world())
                        .filter(|p| p.is_finite())
                        .ok_or_else(|| "Surfd brush references an invalid face vertex".to_owned())
                })
                .collect::<Result<_, _>>()?;
            out.push(Surface {
                vertices,
                color,
                texture: None,
                lightmap: None,
            });
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DisplacementTriangle {
    vertices: [Point; 3],
    surface_id: Option<u32>,
    triangle_id: Option<u32>,
}

#[derive(Deserialize)]
struct ZoneBounds {
    min: Point,
    max: Point,
    #[serde(default)]
    hulls: Vec<RawBrush>,
}

impl ZoneBounds {
    fn add(&self, kind: ZoneKind, out: &mut Vec<Zone>) -> Result<(), String> {
        let a = self.min.precise_world();
        let b = self.max.precise_world();
        let min = a.min(b);
        let max = a.max(b);
        if !min.is_finite()
            || !max.is_finite()
            || !min.as_vec3().is_finite()
            || !max.as_vec3().is_finite()
        {
            return Err("Invalid Surfd zone bounds".into());
        }
        if self.hulls.is_empty() {
            let mut brush = Brush::box_at(min.as_vec3(), max.as_vec3());
            brush.planes = brush.planes.with_zone_geometry(AuthoredZoneGeometry {
                min,
                max,
                planes: Vec::new(),
            });
            out.push(Zone { brush, kind });
        } else {
            for hull in &self.hulls {
                let mut brush = hull.convert()?;
                brush.planes = brush.planes.with_zone_geometry(AuthoredZoneGeometry {
                    min,
                    max,
                    planes: hull
                        .planes
                        .iter()
                        .map(|plane| (plane.normal.precise_world(), plane.distance))
                        .collect(),
                });
                out.push(Zone {
                    brush,
                    kind: kind.clone(),
                });
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct RawZone {
    #[serde(flatten)]
    bounds: ZoneBounds,
    kind: String,
    order: Option<u16>,
}

impl RawZone {
    fn add(&self, out: &mut Vec<Zone>) -> Result<(), String> {
        let kind = match self.kind.as_str() {
            "start" => ZoneKind::Start,
            "finish" => ZoneKind::Finish,
            "reset" => ZoneKind::Reset,
            "checkpoint" => {
                ZoneKind::Checkpoint(self.order.ok_or("Missing Surfd checkpoint order")?)
            }
            _ => return Ok(()),
        };
        self.bounds.add(kind, out)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Teleport {
    #[serde(flatten)]
    bounds: ZoneBounds,
    destination: Option<Spawn>,
    stage: Option<u16>,
    #[serde(default)]
    keep_view: bool,
    #[serde(default)]
    relative: bool,
    #[serde(default)]
    checkpoint: bool,
    #[serde(default)]
    restart: bool,
    landmark: Option<Point>,
    filter: Option<RawFilter>,
    filter_multi: Option<RawPredicate>,
    #[serde(default)]
    carry_push: bool,
    velocity: Option<String>,
}

impl Teleport {
    fn add(
        &self,
        destination: Option<Spawn>,
        zones: &mut Vec<Zone>,
        extended: bool,
    ) -> Result<(), String> {
        let Some(destination) = destination.or_else(|| {
            self.restart.then_some(Spawn {
                position: Point {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                yaw: 0.0,
                pitch: 0.0,
            })
        }) else {
            return Ok(());
        };
        let destination = destination.practice(String::new())?;
        self.bounds.add(
            ZoneKind::AuthoredTeleport(Box::new(AuthoredTeleport {
                position: destination.position,
                yaw: destination.yaw,
                pitch: destination.pitch,
                source_yaw_degrees: destination.source_yaw_degrees,
                landmark: self.landmark.map(Point::world),
                relative: self.relative,
                keep_view: self.keep_view,
                stage: self.stage,
                stage_velocity: self.velocity.as_deref() == Some("stage"),
                preserve_velocity: self.velocity.as_deref() == Some("preserve"),
                checkpoint: self.checkpoint,
                restart: self.restart,
                filter: self.filter.as_ref().map(RawFilter::convert),
                filter_multi: self
                    .filter_multi
                    .as_ref()
                    .filter(|_| extended)
                    .map(RawPredicate::convert),
                carry_push: extended && self.carry_push,
            })),
            zones,
        )
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawPredicate {
    Group {
        combine: String,
        filters: Vec<RawPredicate>,
        #[serde(default)]
        negated: bool,
    },
    Name(RawFilter),
}

impl RawPredicate {
    fn convert(&self) -> TriggerPredicate {
        match self {
            Self::Group {
                combine,
                filters,
                negated,
            } => TriggerPredicate::Group {
                all: combine == "and",
                negated: *negated,
                filters: filters.iter().map(Self::convert).collect(),
            },
            Self::Name(filter) => TriggerPredicate::Name(filter.convert()),
        }
    }
}

#[derive(Deserialize)]
struct RawFilter {
    targetname: String,
    #[serde(default)]
    negated: bool,
}

impl RawFilter {
    fn convert(&self) -> TriggerFilter {
        TriggerFilter {
            targetname: self.targetname.clone(),
            negated: self.negated,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPush {
    #[serde(flatten)]
    bounds: ZoneBounds,
    direction: Point,
    speed: f64,
    #[serde(default)]
    spawnflags: u32,
    filter: Option<RawFilter>,
    filter_multi: Option<RawPredicate>,
}

impl RawPush {
    fn add(&self, index: usize, zones: &mut Vec<Zone>, precise: bool) -> Result<(), String> {
        if self.spawnflags & 65 != 0 && self.spawnflags & 128 == 0 {
            let velocity = if precise {
                (self.direction.precise_world() * self.speed).as_vec3()
            } else {
                self.direction.world() * self.speed as f32
            };
            self.bounds.add(ZoneKind::Push { index, velocity }, zones)?;
        }
        Ok(())
    }

    fn predicate(&self) -> Option<TriggerPredicate> {
        self.filter_multi
            .as_ref()
            .map(RawPredicate::convert)
            .or_else(|| {
                self.filter
                    .as_ref()
                    .map(|filter| TriggerPredicate::Name(filter.convert()))
            })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNoJump {
    #[serde(flatten)]
    bounds: ZoneBounds,
    movement_scale: f32,
}

impl RawNoJump {
    fn add(&self, zones: &mut Vec<Zone>) -> Result<(), String> {
        if self.movement_scale > 0.0 && self.movement_scale.is_finite() {
            self.bounds
                .add(ZoneKind::NoJump(self.movement_scale), zones)?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawConveyor {
    brush_ids: Vec<String>,
    velocity: Point,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOutputZone {
    #[serde(flatten)]
    bounds: ZoneBounds,
    filter: Option<RawFilter>,
    filter_multi: Option<RawPredicate>,
    #[serde(default)]
    on_start: Vec<RawOutput>,
    #[serde(default)]
    on_end: Vec<RawOutput>,
    #[serde(default)]
    on_touch: Vec<RawOutput>,
}

impl RawOutputZone {
    fn add(&self, index: usize, zones: &mut Vec<Zone>, extended: bool) -> Result<(), String> {
        let convert = |raw: &RawOutput| raw.convert(extended);
        self.bounds.add(
            ZoneKind::PlayerOutput(Box::new(PlayerOutputZone {
                index,
                on_trigger: None,
                filter: self.filter.as_ref().map(RawFilter::convert),
                filter_multi: self
                    .filter_multi
                    .as_ref()
                    .filter(|_| extended)
                    .map(RawPredicate::convert),
                on_start: self.on_start.iter().filter_map(convert).collect(),
                on_end: self.on_end.iter().filter_map(convert).collect(),
                on_touch: self
                    .on_touch
                    .iter()
                    .filter(|_| extended)
                    .filter_map(convert)
                    .collect(),
            })),
            zones,
        )
    }
}

#[derive(Deserialize)]
struct RawOutput {
    property: String,
    value: serde_json::Value,
    #[serde(default)]
    delay: f64,
    #[serde(default, rename = "suppressJump")]
    suppress_jump: bool,
}

impl RawOutput {
    fn convert(&self, extended: bool) -> Option<PlayerOutput> {
        let value = match self.property.as_str() {
            "targetname" => PlayerOutputValue::TargetName(self.value.as_str()?.to_owned()),
            "velocity" => PlayerOutputValue::Velocity(
                serde_json::from_value::<Point>(self.value.clone())
                    .ok()?
                    .world(),
            ),
            "basevelocity" | "baseVelocity" => PlayerOutputValue::BaseVelocity(
                serde_json::from_value::<Point>(self.value.clone())
                    .ok()?
                    .world(),
            ),
            "gravity" if extended => PlayerOutputValue::Gravity(self.value.as_f64()? as f32),
            "laggedMovement" if extended => PlayerOutputValue::LaggedMovement {
                scale: self.value.as_f64()? as f32,
                suppress_jump: self.suppress_jump,
            },
            _ => return None,
        };
        let delay_ticks = if matches!(
            value,
            PlayerOutputValue::BaseVelocity(_) | PlayerOutputValue::LaggedMovement { .. }
        ) {
            0
        } else {
            (self.delay * TICK_HZ - 1e-6).ceil() as u32
        };
        Some(PlayerOutput { delay_ticks, value })
    }
}

impl Collision {
    fn to_course(&self, fallback_surfaces: bool) -> Result<Course, String> {
        if self.id.is_empty() || self.brushes.is_empty() {
            return Err("Surfd collision is missing its map id or brushes".into());
        }
        let initial = self.spawn.practice("Course start".to_owned())?;
        let mut brushes =
            Vec::with_capacity(self.brushes.len() + self.displacement_triangles.len());
        let mut recorded_brushes = Vec::with_capacity(self.brushes.len());
        let mut surfaces = Vec::new();
        for brush in &self.brushes {
            brushes.push(brush.convert()?);
            recorded_brushes.push(brush.recorded(self.bsp_corner_fix));
            if fallback_surfaces {
                brush.add_surfaces(&mut surfaces)?;
            }
        }
        if !self.displacement_triangles.is_empty() {
            let mut hulls = Vec::with_capacity(self.displacement_triangles.len());
            hulls.extend(self.displacement_triangles.iter().filter_map(|triangle| {
                DisplacementHull::new(
                    triangle.vertices.map(Point::precise_world),
                    triangle.surface_id.zip(triangle.triangle_id),
                )
            }));
            let hulls = Arc::new(hulls);
            for (index, hull) in hulls.iter().enumerate() {
                brushes.push(DisplacementHull::brush(&hulls, index));
                if fallback_surfaces {
                    surfaces.push(Surface {
                        vertices: hull.vertices.map(DVec3::as_vec3).to_vec(),
                        color: [0.83, 0.86, 0.89],
                        texture: None,
                        lightmap: None,
                    });
                }
            }
        }

        let mut zones = Vec::new();
        for zone in &self.zones {
            zone.add(&mut zones)?;
        }
        for teleport in &self.teleport_zones {
            let destination = teleport
                .stage
                .and_then(|n| n.checked_sub(1))
                .and_then(|n| self.stages.get(usize::from(n)))
                .map(|stage| stage.spawn)
                .or(teleport.destination);
            teleport.add(destination, &mut zones, false)?;
        }
        let continuous_stages = self.stage_progression.as_deref() == Some("continuous");
        let mut authored_stage_starts = Vec::with_capacity(self.stages.len());
        for (index, stage) in self.stages.iter().enumerate() {
            let number = u16::try_from(index + 1).map_err(|_| "Too many map stages")?;
            let mut start_zones = Vec::new();
            if let Some(start) = &stage.start {
                start.add(ZoneKind::StageStart(number), &mut start_zones)?;
            }
            authored_stage_starts.push(start_zones.iter().map(|zone| zone.brush.clone()).collect());
            if !continuous_stages && !self.boosted_start_stages.contains(&number) {
                zones.extend(start_zones);
            }
        }
        for (index, push) in self.push_zones.iter().enumerate() {
            push.add(index, &mut zones, false)?;
        }
        for no_jump in &self.no_jump_zones {
            no_jump.add(&mut zones)?;
        }
        for (index, output) in self.player_output_zones.iter().enumerate() {
            output.add(index, &mut zones, false)?;
        }
        let brush_ids = self
            .brushes
            .iter()
            .enumerate()
            .filter_map(|(index, brush)| brush.id.as_deref().map(|id| (id, index)))
            .collect::<HashMap<_, _>>();
        let conveyors = self
            .conveyors
            .iter()
            .filter_map(|raw| {
                let brush_indices = raw
                    .brush_ids
                    .iter()
                    .filter_map(|id| brush_ids.get(id.as_str()).copied())
                    .collect::<Vec<_>>();
                (!brush_indices.is_empty()).then_some(Conveyor {
                    brush_indices,
                    velocity: raw.velocity.world(),
                })
            })
            .collect();

        let mut practice_spawns = Vec::new();
        if self.practice_stations.is_empty() {
            for (i, stage) in self.stages.iter().enumerate() {
                practice_spawns.push(stage.spawn.practice(format!("Stage {}", i + 1))?);
            }
        } else {
            for station in &self.practice_stations {
                practice_spawns.push(station.spawn.practice(station.name.clone())?);
            }
        }
        for (i, spawn) in self.practice_spawns.iter().enumerate() {
            if spawn.position.world() != initial.position
                && !practice_spawns
                    .iter()
                    .any(|point| point.position == spawn.position.world())
            {
                practice_spawns.push(spawn.practice(format!("Station {}", i + 1))?);
            }
        }
        let kill_y = self.fall_z.unwrap_or_else(|| {
            brushes
                .iter()
                .map(|brush| brush.min.y)
                .fold(initial.position.y, f32::min)
                - 1024.0
        });
        if !kill_y.is_finite() {
            return Err("Invalid Surfd fall height".into());
        }
        Ok(Course {
            name: self.id.clone(),
            catalog_revision: None,
            bonus_routes: Vec::new(),
            map_triggers: MapTriggers {
                source_timed_triggers: false,
                source_world_limit: None,
                source_mode: None,
                spawn_pitch: Some(initial.pitch),
                spawn_source_yaw: initial.source_yaw_degrees,
                brush_index: Default::default(),
                continuous_stages,
                boosted_start_stages: self.boosted_start_stages.clone(),
                push_after_movement: self.push_timing.as_deref() == Some("after-movement"),
                source_push_ground_fix: self.source_push_ground_fix,
                output_velocity_ground_fix: self.output_velocity_ground_fix,
                stage_handoff_releases_duck: self.stage_handoff_releases_duck,
                vphysics_feature_fix: self.vphysics_feature_fix
                    || matches!(
                        (self.id.as_str(), self.version.as_deref()),
                        ("surf_pantheon", Some("bsp-87aa1a9b-contacts-v3"))
                            | ("surf_cyberwave", Some("bsp-96df9092-contacts-v1"))
                    ),
                vphysics_contact_format: if self.id == "surf_pantheon"
                    && self.version.as_deref() == Some("bsp-87aa1a9b-contacts-v3")
                {
                    6
                } else {
                    7
                },
                bsp_corner_fix: self.bsp_corner_fix,
                surfd_rules: true,
                telehop_fix: self.telehop_fix,
                conveyors,
                recorded_brushes,
                source_version: self.version.clone(),
                source_timer: None,
                bonus_pack_sha256: None,
                bonus_rules: HashMap::new(),
                movement_volumes: movement_volumes(&self.ladder_brush_ids, &self.water_zones)?,
                authored_stage_starts,
                authored_stage_spawns: self
                    .stages
                    .iter()
                    .enumerate()
                    .map(|(index, stage)| stage.spawn.practice(format!("Stage {}", index + 1)))
                    .collect::<Result<_, _>>()?,
                surface_friction: self.brushes.iter().map(RawBrush::friction).collect(),
            },
            spawn: initial.position,
            spawn_yaw: initial.yaw,
            brushes,
            surfaces,
            textures: Vec::new(),
            visual_meshes: Vec::new(),
            visual_visibility: None,
            practice_spawns,
            zones,
            kill_y,
            sky: None,
            sky_occluders: Vec::new(),
            fog: None,
        })
    }
}

fn bounds(min: Point, max: Point) -> (Vec3, Vec3) {
    let (a, b) = (min.world(), max.world());
    (a.min(b), a.max(b))
}

fn hex_color(value: &str) -> Option<[f32; 3]> {
    let value = value.strip_prefix('#')?;
    if value.len() != 6 {
        return None;
    }
    let rgb = u32::from_str_radix(value, 16).ok()?;
    Some([16, 8, 0].map(|shift| ((rgb >> shift) & 255) as f32 / 255.0))
}

fn source_axes(point: Vec3) -> Vec3 {
    Vec3::new(point.x, point.z, -point.y)
}

fn read_json<T: DeserializeOwned>(path: &Path, limit: usize) -> Result<T, String> {
    read_revision(path, limit, None)
}

fn read_revision<T: DeserializeOwned>(
    path: &Path,
    limit: usize,
    revision: Option<&str>,
) -> Result<T, String> {
    let bytes = read_asset(path, limit)?;
    if let Some(revision) = revision
        && !format!("{:x}", Sha256::digest(&bytes)).eq_ignore_ascii_case(revision)
    {
        return Err(format!("{}: map asset checksum mismatch", path.display()));
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

fn read_asset(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    with_asset_reader(path, |reader| {
        let mut bytes = Vec::new();
        reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(std::io::Error::other(
                "decompressed Surfd asset is too large",
            ));
        }
        Ok(bytes)
    })
}

fn read_asset_exact(path: &Path, length: usize) -> Result<Vec<u8>, String> {
    with_asset_reader(path, |reader| {
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes)?;
        if reader.read(&mut [0])? != 0 {
            return Err(std::io::Error::other(
                "visual pack exceeds its declared size",
            ));
        }
        Ok(bytes)
    })
}

fn with_asset_reader<T>(
    path: &Path,
    read: impl FnOnce(&mut dyn Read) -> std::io::Result<T>,
) -> Result<T, String> {
    let run = || {
        let mut file = BufReader::new(File::open(path)?);
        if file.fill_buf()?.starts_with(&[0x1f, 0x8b]) {
            read(&mut MultiGzDecoder::new(file))
        } else {
            read(&mut file)
        }
    };
    run().map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::{MoveInput, Player, step};

    #[test]
    fn displacement_triangle_supports_the_player_hull() {
        let hull = DisplacementHull::new(
            [
                DVec3::new(-128.0, 0.0, -128.0),
                DVec3::new(-128.0, 0.0, 128.0),
                DVec3::new(128.0, 0.0, -128.0),
            ],
            None,
        )
        .expect("triangle hull");
        let brush = DisplacementHull::brush(&Arc::new(vec![hull]), 0);
        let mut player = Player::new(Vec3::new(-32.0, 40.0, -32.0));
        for _ in 0..60 {
            step(
                &mut player,
                MoveInput::default(),
                std::slice::from_ref(&brush),
            );
        }
        assert!(player.grounded);
        assert!(
            (0.0..2.0).contains(&player.position.y),
            "{:?}",
            player.position
        );
    }
}
