//! Local timer data bound to an exact Source BSP revision.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read};
use std::path::Path;

use bevy_math::{DVec2, Vec2, Vec3};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{Entity, Geometry, lookup, parse_vector, sidecar_path, source_axes};
use crate::catalog::{MapMode, SourceBspEntry};
use crate::course::{
    BonusRoute, Brush, Course, Plane, PlayerOutput, PlayerOutputValue, PlayerOutputZone,
    PracticeSpawn, Zone, ZoneKind,
};

const MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Timer {
    Shavit,
    Momentum,
}

pub(crate) struct SourceTimer {
    pub(crate) revision: [u8; 32],
    pub(crate) max_velocity: Option<f32>,
    mode: MapMode,
    tracks: BTreeMap<u16, Track>,
    bhop_regions: Vec<(Option<u16>, Brush)>,
}

impl SourceTimer {
    pub(crate) fn mode(&self) -> MapMode {
        self.mode
    }

    pub(crate) fn track(&self, bonus: Option<u16>) -> &Track {
        &self.tracks[&bonus.unwrap_or(0)]
    }

    pub(crate) fn auto_bhop(&self, bonus: Option<u16>, touches: impl Fn(&Brush) -> bool) -> bool {
        let track = bonus.unwrap_or(0);
        self.tracks[&track].bhop_enabled
            || self
                .bhop_regions
                .iter()
                .any(|(id, brush)| id.is_none_or(|id| id == track) && touches(brush))
    }

    pub(crate) fn can_prime(
        &self,
        bonus: Option<u16>,
        position: Vec3,
        grounded: bool,
        touches: impl Fn(&Brush) -> bool,
    ) -> bool {
        self.track(bonus)
            .start_regions
            .iter()
            .any(|(brush, safe_height)| {
                touches(brush)
                    && safe_height.is_none_or(|height| {
                        grounded && position.y <= brush.min.y + height + 0.0625
                    })
            })
    }
}

#[derive(Deserialize)]
pub(super) struct Sidecar {
    version: u8,
    bsp_sha256: String,
    timer: Timer,
    mode: MapMode,
    tier: Option<u8>,
    max_velocity: Option<f32>,
    tracks: Vec<Track>,
    zones: Vec<Region>,
    #[serde(default)]
    entity_output_fixes: Vec<OutputFix>,
    #[serde(skip)]
    revision: [u8; 32],
}

#[derive(Deserialize)]
pub(crate) struct Track {
    id: u16,
    pub(crate) checkpoints_required: bool,
    pub(crate) checkpoints_ordered: bool,
    pub(crate) limit_start_ground_speed: bool,
    bhop_enabled: bool,
    #[serde(skip)]
    start_regions: Vec<(Brush, Option<f32>)>,
}

#[derive(Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RegionKind {
    Start,
    End,
    Stage,
    Checkpoint,
    AllowBhop,
}

#[derive(Deserialize)]
struct Region {
    kind: RegionKind,
    track: Option<u16>,
    index: Option<u16>,
    points: Vec<[f32; 2]>,
    bottom: f32,
    height: f32,
    safe_height: Option<f32>,
    target: Option<String>,
    hook_target: Option<String>,
    hook_hammerid: Option<u32>,
}

#[derive(Deserialize)]
struct OutputFix {
    from_key: String,
    to_key: String,
    value: String,
}

impl Sidecar {
    pub(super) fn is_bhop(&self) -> bool {
        self.mode == MapMode::Bhop
    }

    pub(super) fn inspect(path: &Path) -> Option<(MapMode, Option<u8>, String)> {
        let file = File::open(sidecar_path(path, "surf.json")).ok()?;
        if file.metadata().ok()?.len() > MAX_BYTES {
            return None;
        }
        let timer: Self = serde_json::from_reader(BufReader::new(file.take(MAX_BYTES))).ok()?;
        timer.validate_layout().ok()?;
        Some((timer.mode, timer.tier, timer.bsp_sha256))
    }

    fn validate_layout(&self) -> Result<(), String> {
        #[derive(Default)]
        struct Layout {
            start: bool,
            finish: bool,
            checkpoints: BTreeSet<u16>,
            stages: BTreeSet<u16>,
        }

        if self.version != 1 {
            return Err(format!("Unsupported timer data version {}", self.version));
        }
        if self.bsp_sha256.len() != 64
            || !self.bsp_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("Invalid timer BSP checksum".into());
        }
        let mut layouts = BTreeMap::new();
        for track in &self.tracks {
            if layouts.insert(track.id, Layout::default()).is_some() {
                return Err("Duplicate imported track number".into());
            }
        }
        if !layouts.contains_key(&0) {
            return Err("Imported timer data has no main track".into());
        }
        for zone in &self.zones {
            let id = zone.track.unwrap_or(0);
            let layout = layouts
                .get_mut(&id)
                .ok_or_else(|| format!("Imported region uses unknown track {id}"))?;
            match zone.kind {
                RegionKind::Start => layout.start = true,
                RegionKind::End => layout.finish = true,
                RegionKind::Checkpoint => {
                    layout.checkpoints.insert(zone.order(1)?);
                }
                RegionKind::Stage => {
                    layout.stages.insert(zone.order(2)?);
                }
                RegionKind::AllowBhop => {}
            }
        }
        for (id, layout) in layouts {
            if !layout.start || !layout.finish {
                return Err(format!(
                    "Imported track {id} needs both start and finish zones"
                ));
            }
            if !layout
                .checkpoints
                .into_iter()
                .zip(1..)
                .all(|(actual, expected)| actual == expected)
                || !layout
                    .stages
                    .into_iter()
                    .zip(2..)
                    .all(|(actual, expected)| actual == expected)
            {
                return Err(format!(
                    "Imported track {id} has missing checkpoints or stages"
                ));
            }
        }
        Ok(())
    }

    pub(super) fn read(path: &Path, bsp: &[u8]) -> Result<Option<Self>, String> {
        let path = sidecar_path(path, "surf.json");
        let result = (|| {
            let file = match File::open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.to_string()),
            };
            let mut bytes = Vec::new();
            file.take(MAX_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() as u64 > MAX_BYTES {
                return Err("Timer data exceeds 2 MiB".into());
            }
            let mut timer: Self =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            timer.validate_layout()?;
            if !timer
                .bsp_sha256
                .eq_ignore_ascii_case(&format!("{:x}", Sha256::digest(bsp)))
            {
                return Err("Timer data SHA-256 does not match this BSP revision".into());
            }
            timer.revision = Sha256::digest(&bytes).into();
            Ok(Some(timer))
        })();
        result.map_err(|error: String| format!("{}: {error}", path.display()))
    }

    pub(super) fn verify_ranked(
        &self,
        expected: &SourceBspEntry,
        mode: MapMode,
    ) -> Result<(), String> {
        if self.mode != mode
            || !self.bsp_sha256.eq_ignore_ascii_case(&expected.bsp_sha256)
            || !format!("{:x}", sha2::digest::Output::<Sha256>::from(self.revision))
                .eq_ignore_ascii_case(&expected.timer_sha256)
        {
            return Err("Installed BSP or timer data differs from the ranked map revision".into());
        }
        Ok(())
    }

    pub(super) fn apply(
        self,
        course: &mut Course,
        geometry: &Geometry,
        entities: &[Entity<'_>],
    ) -> Result<(), String> {
        if self
            .max_velocity
            .is_some_and(|value| !(260.0..=32_768.0).contains(&value))
        {
            return Err("Invalid imported max velocity".into());
        }
        let mut tracks = BTreeMap::new();
        for track in self.tracks {
            if tracks.insert(track.id, track).is_some() {
                return Err("Duplicate imported track number".into());
            }
        }
        if !tracks.contains_key(&0) {
            return Err("Imported timer data has no main track".into());
        }

        add_base_velocity_outputs(
            &mut course.zones,
            &self.entity_output_fixes,
            geometry,
            entities,
        )?;
        let effects = std::mem::take(&mut course.zones);
        let mut routes = BTreeMap::<u16, Vec<Zone>>::new();
        let mut starts = BTreeMap::<u16, PracticeSpawn>::new();
        let mut stages = BTreeMap::<u16, (Vec<Brush>, Option<PracticeSpawn>)>::new();
        let mut bhop_regions = Vec::new();
        for region in self.zones {
            if region
                .safe_height
                .is_some_and(|height| height != -1.0 && !(0.0..=region.height).contains(&height))
            {
                return Err("Invalid imported safe height".into());
            }
            let track = region.track.unwrap_or(0);
            let rules = tracks
                .get_mut(&track)
                .ok_or_else(|| format!("Imported region uses unknown track {track}"))?;
            let brushes = region.brushes(geometry, entities)?;
            if region.kind == RegionKind::AllowBhop {
                bhop_regions.extend(brushes.into_iter().map(|brush| (region.track, brush)));
                continue;
            }
            let kind = match region.kind {
                RegionKind::Start => {
                    rules.start_regions.extend(
                        brushes.iter().cloned().map(|brush| {
                            (brush, region.safe_height.filter(|height| *height >= 0.0))
                        }),
                    );
                    if let Some(target) = &region.target {
                        let spawn = spawn_target(entities, target, format!("Bonus {track}"))?;
                        if !brushes
                            .iter()
                            .any(|brush| near_start(brush, spawn.position))
                        {
                            return Err(format!(
                                "Imported start target {target} is outside track {track}'s start"
                            ));
                        }
                        if starts.insert(track, spawn).is_some() {
                            return Err(format!(
                                "Imported track {track} has multiple respawn destinations"
                            ));
                        }
                    }
                    ZoneKind::Start
                }
                RegionKind::End => ZoneKind::Finish,
                RegionKind::Checkpoint => ZoneKind::Checkpoint(region.order(1)?),
                RegionKind::Stage => {
                    let number = region.order(2)?;
                    if track != 0 {
                        return Err("Imported stages are only supported on the main track".into());
                    }
                    if !rules.checkpoints_required || !rules.checkpoints_ordered {
                        return Err("Imported stages must be required and ordered".into());
                    }
                    let stage = stages.entry(number).or_default();
                    stage.0.extend(brushes.iter().cloned());
                    if let Some(target) = &region.target {
                        stage.1 = Some(spawn_target(entities, target, format!("Stage {number}"))?);
                    }
                    routes
                        .entry(track)
                        .or_default()
                        .extend(brushes.iter().cloned().map(|brush| Zone {
                            brush,
                            kind: ZoneKind::StageStart(number),
                        }));
                    ZoneKind::Checkpoint(number - 1)
                }
                RegionKind::AllowBhop => unreachable!(),
            };
            routes
                .entry(track)
                .or_default()
                .extend(brushes.into_iter().map(|brush| Zone {
                    brush,
                    kind: kind.clone(),
                }));
        }

        for &id in tracks.keys() {
            let mut zones = routes.remove(&id).unwrap_or_default();
            let has =
                |predicate: fn(&ZoneKind) -> bool| zones.iter().any(|zone| predicate(&zone.kind));
            if !has(|kind| matches!(kind, ZoneKind::Start))
                || !has(|kind| matches!(kind, ZoneKind::Finish))
            {
                return Err(format!(
                    "Imported track {id} needs both start and finish zones"
                ));
            }
            zones.extend(effects.iter().cloned());
            let contains_start = |position| {
                zones.iter().any(|zone| {
                    matches!(zone.kind, ZoneKind::Start) && near_start(&zone.brush, position)
                })
            };
            let mut spawn = starts.remove(&id);
            if spawn.is_none()
                && matches!(self.timer, Timer::Shavit)
                && (id != 0 || !contains_start(course.spawn))
            {
                let name = if id == 0 {
                    "Start".into()
                } else {
                    format!("Bonus {id}")
                };
                spawn = unique_map_spawn(entities, &name, contains_start);
            }
            if id == 0 {
                if let Some(spawn) = spawn {
                    course.spawn = spawn.position;
                    course.spawn_yaw = spawn.yaw;
                    course.map_triggers.spawn_pitch = Some(spawn.pitch);
                    course.map_triggers.spawn_source_yaw = spawn.source_yaw_degrees;
                } else if !contains_start(course.spawn) {
                    return Err(
                        "The BSP spawn is outside the imported main start. Supply a start target."
                            .into(),
                    );
                }
                course.zones = zones;
            } else {
                let spawn =
                    spawn.ok_or_else(|| format!("Imported bonus {id} has no spawn target"))?;
                course.bonus_routes.push(BonusRoute {
                    number: id,
                    name: spawn.name.clone(),
                    spawn,
                    zones,
                    revision: format!("{:x}", Sha256::digest(self.revision)),
                });
            }
        }

        if !stages.is_empty() {
            let starts = &mut course.map_triggers.authored_stage_starts;
            starts.push(Vec::new());
            for (number, (brushes, mut spawn)) in stages {
                if usize::from(number) != starts.len() + 1 {
                    return Err("Imported stages must be numbered consecutively from 2".into());
                }
                if spawn.is_none() && matches!(self.timer, Timer::Shavit) {
                    spawn = unique_map_spawn(entities, &format!("Stage {number}"), |position| {
                        brushes.iter().any(|brush| near_start(brush, position))
                    });
                }
                if let Some(spawn) = spawn {
                    if !brushes
                        .iter()
                        .any(|brush| near_start(brush, spawn.position))
                    {
                        return Err(format!(
                            "Imported stage {number} spawn is outside the start"
                        ));
                    }
                    course.practice_spawns.push(spawn);
                }
                starts.push(brushes);
            }
            course.map_triggers.continuous_stages = true;
            if course.practice_spawns.len() + 1 == course.map_triggers.authored_stage_starts.len() {
                course
                    .map_triggers
                    .authored_stage_spawns
                    .push(PracticeSpawn {
                        name: "Stage 1".into(),
                        position: course.spawn,
                        yaw: course.spawn_yaw,
                        pitch: course.map_triggers.spawn_pitch.unwrap_or(0.0),
                        source_yaw_degrees: course.map_triggers.spawn_source_yaw,
                    });
                course
                    .map_triggers
                    .authored_stage_spawns
                    .extend(course.practice_spawns.iter().cloned());
            }
        }
        course.map_triggers.source_timer = Some(SourceTimer {
            revision: self.revision,
            max_velocity: self.max_velocity.or_else(|| {
                matches!((self.timer, self.mode), (Timer::Momentum, MapMode::Surf))
                    .then_some(3500.0)
            }),
            mode: self.mode,
            tracks,
            bhop_regions,
        });
        Ok(())
    }
}

fn near_start(brush: &Brush, position: Vec3) -> bool {
    (brush.min - Vec3::Y).cmple(position).all()
        && (brush.max + Vec3::Y * 32.0).cmpge(position).all()
        && brush
            .planes
            .iter()
            .filter(|plane| plane.normal.y == 0.0)
            .all(|plane| plane.normal.dot(position) <= plane.distance + 0.01)
}

fn spawn_target(
    entities: &[Entity<'_>],
    target: &str,
    name: String,
) -> Result<PracticeSpawn, String> {
    let entity = entities
        .iter()
        .find(|entity| {
            matches!(
                entity.get("classname"),
                Some("info_teleport_destination" | "info_target")
            ) && entity
                .get("targetname")
                .is_some_and(|value| value.eq_ignore_ascii_case(target))
        })
        .ok_or_else(|| format!("Imported respawn target {target} was not found in the BSP"))?;
    entity_spawn(entity, &name).ok_or_else(|| format!("Invalid position for {target}"))
}

fn entity_spawn(entity: &Entity<'_>, name: &str) -> Option<PracticeSpawn> {
    Some(PracticeSpawn {
        name: name.into(),
        position: entity.origin()?,
        yaw: entity.yaw(),
        pitch: entity.angles().x.to_radians(),
        source_yaw_degrees: Some(f64::from(entity.angles().y)),
    })
}

fn unique_map_spawn(
    entities: &[Entity<'_>],
    name: &str,
    contains: impl Fn(Vec3) -> bool,
) -> Option<PracticeSpawn> {
    for classes in [
        &["info_teleport_destination"][..],
        &[
            "info_player_start",
            "info_player_counterterrorist",
            "info_player_terrorist",
        ],
    ] {
        let mut spawns = entities.iter().filter_map(|entity| {
            classes
                .contains(&entity.get("classname")?)
                .then(|| entity_spawn(entity, name))
                .flatten()
                .filter(|spawn| contains(spawn.position))
        });
        if let Some(spawn) = spawns.next() {
            return spawns
                .all(|other| other.position == spawn.position && other.yaw == spawn.yaw)
                .then_some(spawn);
        }
    }
    None
}

impl Region {
    fn order(&self, minimum: u16) -> Result<u16, String> {
        self.index
            .filter(|index| *index >= minimum)
            .ok_or_else(|| "Invalid imported checkpoint or stage index".into())
    }

    fn brushes(&self, geometry: &Geometry, entities: &[Entity<'_>]) -> Result<Vec<Brush>, String> {
        if self.hook_target.is_none() && self.hook_hammerid.is_none() {
            return polygon_prisms(&self.points, self.bottom, self.height);
        }
        let mut brushes = Vec::new();
        for entity in entities.iter().filter(|entity| {
            entity.get("classname") == Some("trigger_multiple")
                && entity.get("startdisabled") != Some("1")
                && self.hook_target.as_ref().is_none_or(|target| {
                    entity
                        .get("targetname")
                        .is_some_and(|value| value.eq_ignore_ascii_case(target))
                })
                && self.hook_hammerid.is_none_or(|id| {
                    entity.get("hammerid").and_then(|value| value.parse().ok()) == Some(id)
                })
        }) {
            if let Some(index) = entity.model_index().filter(|&index| index > 0) {
                let model = lookup(&geometry.models, index, "imported zone model")?;
                brushes.extend(geometry.model_brushes(model, entity.placement(), false)?);
            }
        }
        if brushes.is_empty() {
            return Err(format!(
                "Imported zone hook {:?}/{:?} has no brush",
                self.hook_target, self.hook_hammerid
            ));
        }
        Ok(brushes)
    }
}

fn polygon_prisms(points: &[[f32; 2]], bottom: f32, height: f32) -> Result<Vec<Brush>, String> {
    let error = match prism(points, bottom, height) {
        Ok(brush) => return Ok(vec![brush]),
        Err(error) => error,
    };
    if !(3..=64).contains(&points.len()) || !points.iter().flatten().all(|v| v.is_finite()) {
        return Err(error);
    }

    // Keep authored concave regions exact. The run timer already treats a group
    // of overlapping or touching start brushes as one continuous trigger.
    let polygon: Vec<_> = points
        .iter()
        .map(|[x, y]| DVec2::new((*x).into(), (*y).into()))
        .collect();
    let n = polygon.len();
    let cross = |a: DVec2, b: DVec2, c: DVec2| (b - a).perp_dot(c - a);
    let area = (1..n - 1)
        .map(|i| cross(polygon[0], polygon[i], polygon[i + 1]))
        .sum::<f64>();
    if area == 0.0 || (0..n).any(|i| polygon[i + 1..].contains(&polygon[i])) {
        return Err("Imported zone polygon is degenerate".into());
    }
    for i in 0..n {
        let a = polygon[i];
        let b = polygon[(i + 1) % n];
        for j in i + 2..n {
            if i == 0 && j == n - 1 {
                continue;
            }
            let c = polygon[j];
            let d = polygon[(j + 1) % n];
            let bounds_overlap = a.min(b).cmple(c.max(d)).all() && c.min(d).cmple(a.max(b)).all();
            if bounds_overlap
                && cross(a, b, c) * cross(a, b, d) <= 0.0
                && cross(c, d, a) * cross(c, d, b) <= 0.0
            {
                return Err("Imported zone polygon crosses itself".into());
            }
        }
    }

    let winding = area.signum();
    let mut corners: Vec<_> = (0..n).collect();
    let mut brushes = Vec::with_capacity(n - 2);
    while corners.len() > 3 {
        let count = corners.len();
        let ear = (0..count).find_map(|i| {
            let indices = [
                corners[(i + count - 1) % count],
                corners[i],
                corners[(i + 1) % count],
            ];
            let [a, b, c] = indices.map(|index| polygon[index]);
            if winding * cross(a, b, c) <= 0.0 {
                return None;
            }
            let contains_corner = corners.iter().any(|&index| {
                let p = polygon[index];
                !indices.contains(&index)
                    && [(a, b), (b, c), (c, a)]
                        .iter()
                        .all(|&(s, t)| winding * cross(s, t, p) >= 0.0)
            });
            (!contains_corner).then_some((i, indices))
        });
        let (i, indices) = ear.ok_or("Could not partition imported zone polygon")?;
        brushes.push(prism(&indices.map(|index| points[index]), bottom, height)?);
        corners.remove(i);
    }
    let triangle = [points[corners[0]], points[corners[1]], points[corners[2]]];
    brushes.push(prism(&triangle, bottom, height)?);
    Ok(brushes)
}

fn prism(points: &[[f32; 2]], bottom: f32, height: f32) -> Result<Brush, String> {
    let top = bottom + height;
    if !(3..=64).contains(&points.len()) || !bottom.is_finite() || !top.is_finite() || top <= bottom
    {
        return Err("Invalid imported zone prism dimensions".into());
    }
    let points: Vec<_> = points.iter().copied().map(Vec2::from_array).collect();
    let min = points.iter().copied().reduce(Vec2::min).unwrap();
    let max = points.iter().copied().reduce(Vec2::max).unwrap();
    if !points.iter().all(|point| point.is_finite()) || !min.cmplt(max).all() {
        return Err("Invalid imported zone polygon bounds".into());
    }
    let center = points.iter().copied().sum::<Vec2>() / points.len() as f32;
    let mut planes = vec![
        Plane {
            normal: Vec3::Y,
            distance: top,
        },
        Plane {
            normal: Vec3::NEG_Y,
            distance: -bottom,
        },
    ];
    for (&a, &b) in points.iter().zip(points.iter().cycle().skip(1)) {
        let mut normal = (b - a)
            .perp()
            .try_normalize()
            .ok_or("Imported zone polygon repeats a vertex")?;
        if normal.dot(center - a) > 0.0 {
            normal = -normal;
        }
        let distance = normal.dot(a);
        if !distance.is_finite() || points.iter().any(|point| normal.dot(*point - a) > 0.01) {
            return Err("Imported zone polygons must be convex".into());
        }
        planes.push(Plane {
            normal: source_axes(normal.extend(0.0)),
            distance,
        });
    }
    Ok(Brush {
        planes: planes.into(),
        min: Vec3::new(min.x, bottom, -max.y),
        max: Vec3::new(max.x, top, -min.y),
    })
}

#[cfg(test)]
mod polygon_tests {
    use super::*;

    #[test]
    fn concave_polygon_brushes_keep_the_open_corner() {
        let mut corners = [[0., 0.], [4., 0.], [4., 2.], [2., 2.], [2., 4.], [0., 4.]];
        for _ in 0..2 {
            let brushes = polygon_prisms(&corners, 0., 8.).unwrap();
            let covers = |point| brushes.iter().any(|brush| near_start(brush, point));
            assert!(covers(Vec3::new(3., 4., -1.)));
            assert!(covers(Vec3::new(1., 4., -3.)));
            assert!(!covers(Vec3::new(3., 4., -3.)));
            corners.reverse();
        }
    }

    #[test]
    fn crossing_polygon_is_rejected() {
        assert!(polygon_prisms(&[[0., 0.], [4., 4.], [0., 4.], [3., 0.]], 0., 8.).is_err());
    }
}

fn add_base_velocity_outputs(
    zones: &mut Vec<Zone>,
    fixes: &[OutputFix],
    geometry: &Geometry,
    entities: &[Entity<'_>],
) -> Result<(), String> {
    let supported_key = |key: &str| {
        key.eq_ignore_ascii_case("OnStartTouch") || key.eq_ignore_ascii_case("OnEndTouch")
    };
    if fixes
        .iter()
        .any(|fix| !supported_key(&fix.from_key) || !supported_key(&fix.to_key))
    {
        return Err("Imported output fixes must use supported touch events".into());
    }
    let mut matched = BTreeSet::new();
    for (index, entity) in entities.iter().enumerate() {
        if entity.get("classname") != Some("trigger_multiple")
            || entity.get("startdisabled") == Some("1")
            || entity
                .get("filtername")
                .is_some_and(|name| !name.is_empty())
        {
            continue;
        }
        let Some(model) = entity.model_index().filter(|&index| index != 0) else {
            continue;
        };
        let mut output = PlayerOutputZone {
            index,
            filter: None,
            filter_multi: None,
            on_start: Vec::new(),
            on_end: Vec::new(),
            on_touch: Vec::new(),
        };
        for &(mut key, value) in &entity.outputs {
            let Some(velocity) = value
                .strip_prefix("!activator,AddOutput,Basevelocity ")
                .and_then(|value| value.strip_suffix(",0,-1"))
                .and_then(parse_vector)
            else {
                continue;
            };
            if let Some((index, fix)) = fixes
                .iter()
                .enumerate()
                .find(|(_, fix)| fix.from_key.eq_ignore_ascii_case(key) && fix.value == value)
            {
                matched.insert(index);
                key = &fix.to_key;
            }
            let actions = if key.eq_ignore_ascii_case("OnStartTouch") {
                &mut output.on_start
            } else if key.eq_ignore_ascii_case("OnEndTouch") {
                &mut output.on_end
            } else {
                continue;
            };
            actions.push(PlayerOutput {
                delay_ticks: 0,
                value: PlayerOutputValue::BaseVelocity(source_axes(velocity)),
            });
        }
        if !output.on_start.is_empty() || !output.on_end.is_empty() {
            let model = lookup(&geometry.models, model, "output model")?;
            zones.extend(
                geometry
                    .model_brushes(model, entity.placement(), false)?
                    .into_iter()
                    .map(|brush| Zone {
                        brush,
                        kind: ZoneKind::PlayerOutput(Box::new(output.clone())),
                    }),
            );
        }
    }
    if matched.len() != fixes.len() {
        return Err("An imported Stripper output fix did not match a supported BSP output".into());
    }
    Ok(())
}
