//! Local timer data bound to an exact Source BSP revision.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read};
use std::path::Path;

use bevy_math::{DVec2, Vec2, Vec3};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{Entity, Geometry, lookup, parse_vector, sidecar_path, source_axes};
use crate::catalog::{
    CatalogEntry, MapMode, SOURCE_RULES_CSS_BHOP, SOURCE_RULES_MOVEMENT_SETTINGS,
    SOURCE_RULES_NORMALIZED_OUTPUTS, SOURCE_RULES_PLAYER_FILTERS, SOURCE_RULES_TICK_INTERVAL,
    SOURCE_RULES_TIMED_TRIGGERS, valid_source_missing_filters,
};
use crate::course::{
    BonusRoute, Brush, Course, Plane, PlayerOutput, PlayerOutputValue, PlayerOutputZone,
    PracticeSpawn, SourceTeleport, TimedPlayerAction, TimedPlayerOutput, TriggerFilter, Zone,
    ZoneKind,
};
use crate::rules::{SourceMovementSettings, TickInterval};

const MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Timer {
    Shavit,
    Momentum,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceStartPolicy {
    CssBhopV5,
}

pub(crate) struct SourceTimer {
    pub(crate) revision: [u8; 32],
    pub(crate) max_velocity: Option<f32>,
    pub(crate) movement_settings: SourceMovementSettings,
    pub(crate) start_policy: Option<SourceStartPolicy>,
    mode: MapMode,
    tracks: BTreeMap<u16, Track>,
    bhop_regions: Vec<(Option<u16>, Brush)>,
}

impl SourceTimer {
    #[cfg(test)]
    pub(crate) fn test_with_interval(start: Brush, interval: TickInterval) -> Self {
        Self {
            revision: [10; 32],
            max_velocity: None,
            movement_settings: SourceMovementSettings {
                tick_interval_ms: Some(interval),
                ..Default::default()
            },
            start_policy: None,
            mode: MapMode::Bhop,
            tracks: BTreeMap::from([(
                0,
                Track {
                    id: 0,
                    checkpoints_required: true,
                    checkpoints_ordered: true,
                    limit_start_ground_speed: false,
                    start_speed_cap: None,
                    bhop_enabled: true,
                    start_regions: vec![(start, None)],
                },
            )]),
            bhop_regions: Vec::new(),
        }
    }

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
    #[serde(default)]
    source_movement: SourceMovementSettings,
    tracks: Vec<Track>,
    zones: Vec<Region>,
    #[serde(default)]
    entity_output_fixes: Vec<OutputFix>,
    #[serde(default)]
    source_gravity_outputs: bool,
    #[serde(default)]
    source_missing_filters: Vec<String>,
    #[serde(default)]
    source_timed_triggers: bool,
    #[serde(default)]
    source_landmark_teleports: bool,
    #[serde(default)]
    source_player_filters: bool,
    source_prestart_target: Option<String>,
    #[serde(default)]
    source_speedmod: bool,
    source_start_policy: Option<SourceStartPolicy>,
    #[serde(skip)]
    revision: [u8; 32],
}

#[derive(Deserialize)]
pub(crate) struct Track {
    id: u16,
    pub(crate) checkpoints_required: bool,
    pub(crate) checkpoints_ordered: bool,
    pub(crate) limit_start_ground_speed: bool,
    pub(crate) start_speed_cap: Option<f32>,
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

        self.source_movement.validate()?;
        if self.source_movement.landfix == Some(true)
            && (self.mode != MapMode::Bhop || self.source_movement.tick_interval().millis() != 10)
        {
            return Err("Imported landfix requires a 100 Hz Bhop profile".into());
        }
        if self.source_start_policy.is_some()
            && (self.mode != MapMode::Bhop
                || !matches!(self.timer, Timer::Shavit)
                || self.source_movement.tick_interval() != TickInterval::Source10Ms
                || self.tracks.len() != 1
                || !self.tracks[0].bhop_enabled
                || self.tracks[0].limit_start_ground_speed
                || self.zones.iter().any(|zone| {
                    !matches!(zone.kind, RegionKind::Start | RegionKind::End)
                        || zone.safe_height.is_some_and(|height| height >= 0.0)
                }))
        {
            return Err(
                "Imported CSS Bhop timer requires one unrestricted linear 100 Hz track".into(),
            );
        }
        if self.version != 1 {
            return Err(format!("Unsupported timer data version {}", self.version));
        }
        if !valid_source_missing_filters(&self.source_missing_filters) {
            return Err("Invalid imported missing filter names".into());
        }
        if self
            .source_prestart_target
            .as_deref()
            .is_some_and(|target| !literal_target_name(target, true))
        {
            return Err("Imported prestart needs a literal BSP destination".into());
        }
        if self.bsp_sha256.len() != 64
            || !self.bsp_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("Invalid timer BSP checksum".into());
        }
        let mut layouts = BTreeMap::new();
        for track in &self.tracks {
            if track.start_speed_cap.is_some_and(|cap| {
                !track.limit_start_ground_speed || !(1.0..=32_768.0).contains(&cap)
            }) {
                return Err(format!(
                    "Invalid start speed cap for imported track {}",
                    track.id
                ));
            }
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

    pub(super) fn verify_ranked(&self, entry: &CatalogEntry) -> Result<(), String> {
        let expected = entry.source_bsp.as_ref().ok_or("No ranked BSP revision")?;
        if self.mode != entry.mode
            || !self.bsp_sha256.eq_ignore_ascii_case(&expected.bsp_sha256)
            || !format!("{:x}", sha2::digest::Output::<Sha256>::from(self.revision))
                .eq_ignore_ascii_case(&expected.timer_sha256)
        {
            return Err("Installed BSP or timer data differs from the ranked map revision".into());
        }
        if (self.source_gravity_outputs && entry.min_source_rules == 0)
            || (!self.source_missing_filters.is_empty()
                && entry.min_source_rules < SOURCE_RULES_NORMALIZED_OUTPUTS)
            || ((self.source_timed_triggers || self.source_landmark_teleports)
                && entry.min_source_rules < SOURCE_RULES_TIMED_TRIGGERS)
            || (self.source_player_filters && entry.min_source_rules < SOURCE_RULES_PLAYER_FILTERS)
            || (self.source_movement.tick_interval_ms.is_some()
                && entry.min_source_rules < SOURCE_RULES_TICK_INTERVAL)
            || ((self.source_movement.landfix.is_some() || self.source_start_policy.is_some())
                && entry.min_source_rules < SOURCE_RULES_CSS_BHOP)
            || ((self.source_movement.changes_defaults()
                || self.source_prestart_target.is_some()
                || self.source_speedmod
                || self
                    .tracks
                    .iter()
                    .any(|track| track.start_speed_cap.is_some()))
                && entry.min_source_rules < SOURCE_RULES_MOVEMENT_SETTINGS)
            || entry
                .download
                .as_ref()
                .and_then(|download| download.shavit.as_ref())
                .is_some_and(|shavit| {
                    (shavit.gravity_outputs
                        && entry.min_source_rules < SOURCE_RULES_NORMALIZED_OUTPUTS)
                        || shavit.gravity_outputs != self.source_gravity_outputs
                        || shavit.missing_filters != self.source_missing_filters
                        || shavit.timed_triggers != self.source_timed_triggers
                        || shavit.landmark_teleports != self.source_landmark_teleports
                        || shavit.player_filters != self.source_player_filters
                        || shavit
                            .tick_interval_ms
                            .map(TickInterval::try_from)
                            .transpose()
                            != Ok(self.source_movement.tick_interval_ms)
                })
        {
            return Err("Imported entity outputs differ from the ranked map rules".into());
        }
        Ok(())
    }

    pub(super) fn apply(
        self,
        course: &mut Course,
        geometry: &Geometry,
        entities: &[Entity<'_>],
        source_rules: u8,
    ) -> Result<(), String> {
        if self
            .max_velocity
            .is_some_and(|value| !(260.0..=32_768.0).contains(&value))
        {
            return Err("Invalid imported max velocity".into());
        }
        let extended_names = source_rules >= SOURCE_RULES_PLAYER_FILTERS;
        add_player_outputs(&self, &mut course.zones, geometry, entities, extended_names)?;
        add_source_teleports(
            &mut course.zones,
            geometry,
            entities,
            self.source_timed_triggers || self.source_player_filters,
            self.source_landmark_teleports,
            extended_names,
        )?;
        course.map_triggers.source_timed_triggers =
            self.source_timed_triggers || self.source_player_filters;
        let mut tracks = BTreeMap::new();
        for track in self.tracks {
            if tracks.insert(track.id, track).is_some() {
                return Err("Duplicate imported track number".into());
            }
        }
        if !tracks.contains_key(&0) {
            return Err("Imported timer data has no main track".into());
        }
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
            if id == 0
                && let Some(target) = &self.source_prestart_target
            {
                let entity = unique_source_entity(
                    entities,
                    target,
                    &["info_teleport_destination"],
                    extended_names,
                )?;
                let prestart = entity_spawn(entity, "Start")
                    .ok_or_else(|| format!("Invalid prestart destination {target}"))?;
                if spawn.is_some() || contains_start(prestart.position) {
                    return Err(
                        "Imported prestart must be separate from the main timer start".into(),
                    );
                }
                spawn = Some(prestart);
            }
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
            movement_settings: self.source_movement,
            start_policy: self.source_start_policy,
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

fn add_player_outputs(
    timer: &Sidecar,
    zones: &mut Vec<Zone>,
    geometry: &Geometry,
    entities: &[Entity<'_>],
    extended_names: bool,
) -> Result<(), String> {
    let fixes = &timer.entity_output_fixes;
    let gravity_outputs = timer.source_gravity_outputs;
    let tick_hz = entity_tick_hz(timer.source_movement.tick_interval());
    if fixes
        .iter()
        .any(|fix| touch_event(&fix.from_key).is_none() || touch_event(&fix.to_key).is_none())
    {
        return Err("Imported output fixes must use supported touch events".into());
    }
    let missing_filter_boosters = missing_filter_boosters(&timer.source_missing_filters, entities)?;
    let speedmod = timer
        .source_speedmod
        .then(|| source_speedmod_target(entities))
        .transpose()?;
    let mut speedmod_count = 0;
    let mut matched = BTreeSet::new();
    let mut timed_count = 0;
    let mut name_count = 0;
    for (index, entity) in entities.iter().enumerate() {
        let model = entity.model_index().filter(|&index| index != 0);
        let gravity_brush = gravity_outputs && entity.get("classname") == Some("trigger_gravity");
        let touch_trigger = entity.get("classname") == Some("trigger_multiple");
        let model = model.filter(|_| {
            (touch_trigger || gravity_brush)
                && entity.get("startdisabled") != Some("1")
                && (entity.get("filtername").is_none_or(str::is_empty)
                    || missing_filter_boosters.contains(&index))
        });
        let static_client = gravity_outputs && static_client_trigger(entity);
        let brush_gravity = if gravity_brush {
            if model.is_none() || !static_client {
                return Err(
                    "Imported gravity brush needs an enabled, unfiltered client trigger".into(),
                );
            }
            Some(parse_gravity(entity.get("gravity").unwrap_or("0"))?)
        } else {
            None
        };
        let mut output = PlayerOutputZone {
            index,
            filter: None,
            filter_multi: None,
            on_start: Vec::new(),
            on_end: Vec::new(),
            on_touch: brush_gravity
                .map(|gravity| PlayerOutput {
                    delay_ticks: 0,
                    value: PlayerOutputValue::Gravity(gravity),
                })
                .into_iter()
                .collect(),
            on_trigger: None,
        };
        let speedmod_touch = speedmod
            .map(|target| source_speedmod_touch(entity, target))
            .transpose()?
            .flatten();
        if let Some(scale) = speedmod_touch {
            let action = |scale| PlayerOutput {
                delay_ticks: 0,
                value: PlayerOutputValue::LaggedMovement {
                    scale,
                    suppress_jump: true,
                },
            };
            output.on_start.push(action(scale));
            output.on_end.push(action(1.0));
            speedmod_count += 1;
        }
        let mut triggered = Vec::new();
        for &(mut key, value) in &entity.outputs {
            if timer.source_timed_triggers && key.eq_ignore_ascii_case("OnTrigger") {
                if let Some(action) = parse_player_output(value, gravity_outputs, true, tick_hz)? {
                    if !matches!(
                        action.value,
                        PlayerOutputValue::TargetName(_) | PlayerOutputValue::Gravity(_)
                    ) {
                        return Err("Unsupported imported timed player action".into());
                    }
                    let delimiter = if value.contains('\x1b') { '\x1b' } else { ',' };
                    let delay = value
                        .split(delimiter)
                        .nth(3)
                        .ok_or("Imported timed output has no delay")?;
                    let seconds = parse_trigger_seconds(delay)?;
                    let delay_from_tick_ns = ((seconds - f64::from(action.delay_ticks) / tick_hz)
                        * 1_000_000_000.0)
                        .round() as i32;
                    triggered.push(TimedPlayerAction {
                        delay_from_tick_ns,
                        output: action,
                    });
                }
                continue;
            }
            let Some(action) =
                parse_player_output(value, gravity_outputs, timer.source_player_filters, tick_hz)?
            else {
                continue;
            };
            let gravity = matches!(action.value, PlayerOutputValue::Gravity(_));
            let player_name = matches!(action.value, PlayerOutputValue::TargetName(_));
            if player_name {
                if !touch_trigger
                    || !static_unnamed_trigger(entity)
                    || entity.outputs.len() != 1
                    || entity
                        .get("filtername")
                        .is_some_and(|name| !name.is_empty())
                    || touch_event(key) != Some(true)
                    || action.delay_ticks != 0
                {
                    return Err(
                        "Imported player selector must be a static OnStartTouch name setter".into(),
                    );
                }
                name_count += 1;
            }
            if let Some(brush_gravity) = brush_gravity {
                if gravity
                    && !(touch_event(key) == Some(true)
                        && action.delay_ticks == 0
                        && matches!(action.value, PlayerOutputValue::Gravity(value) if value == brush_gravity))
                {
                    return Err("Imported gravity brush has a conflicting gravity output".into());
                }
                continue;
            }
            if model.is_none() || (gravity && !static_client) {
                if gravity {
                    return Err(
                        "Imported gravity output needs an enabled, unfiltered client trigger"
                            .into(),
                    );
                }
                continue;
            }
            if !gravity
                && !player_name
                && let Some((index, fix)) = fixes
                    .iter()
                    .enumerate()
                    .find(|(_, fix)| fix.from_key.eq_ignore_ascii_case(key) && fix.value == value)
            {
                matched.insert(index);
                key = &fix.to_key;
            }
            match touch_event(key) {
                Some(true) => output.on_start.push(action),
                Some(false) => output.on_end.push(action),
                None if gravity => return Err("Imported gravity must use a touch event".into()),
                None => {}
            }
        }
        if !triggered.is_empty() {
            if extended_names
                && touch_trigger
                && entity.get("model").is_none()
                && static_unnamed_client(entity)
                && entity.get("filtername").is_none_or(str::is_empty)
                && entity.outputs.len() == triggered.len()
            {
                continue;
            }
            if !static_unnamed_trigger(entity)
                || !touch_trigger
                || entity
                    .get("filtername")
                    .is_some_and(|name| !name.is_empty())
                || entity.outputs.len() != triggered.len()
            {
                return Err("Imported timed player actions need a static trigger_multiple".into());
            }
            let repeat_ticks = trigger_wait_ticks(entity.get("wait").unwrap_or("0"), tick_hz)?;
            output.on_trigger = Some(TimedPlayerOutput {
                repeat_ticks,
                actions: triggered,
            });
            timed_count += 1;
        }
        if let Some(model) = model
            && (!output.on_start.is_empty()
                || !output.on_end.is_empty()
                || !output.on_touch.is_empty()
                || output.on_trigger.is_some())
        {
            let model = lookup(&geometry.models, model, "output model")?;
            let brushes = geometry.model_brushes(model, entity.placement(), false)?;
            if brushes.is_empty()
                && (missing_filter_boosters.contains(&index)
                    || output.on_trigger.is_some()
                    || speedmod_touch.is_some())
            {
                return Err("Imported player output has no brush".into());
            }
            zones.extend(brushes.into_iter().map(|brush| Zone {
                brush,
                kind: ZoneKind::PlayerOutput(Box::new(output.clone())),
            }));
        }
    }
    if speedmod.is_some() && speedmod_count == 0 {
        return Err("Imported Source speed modifier has no touch triggers".into());
    }
    if matched.len() != fixes.len() {
        return Err("An imported Stripper output fix did not match a supported BSP output".into());
    }
    if timer.source_timed_triggers && timed_count == 0 {
        return Err("No supported timed player triggers were found in the BSP".into());
    }
    if timer.source_player_filters && name_count == 0 {
        return Err("No supported player name selectors were found in the BSP".into());
    }
    Ok(())
}

fn missing_filter_boosters(
    names: &[String],
    entities: &[Entity<'_>],
) -> Result<BTreeSet<usize>, String> {
    if names.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut unresolved = names.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let mut boosters = BTreeSet::new();
    for (index, entity) in entities.iter().enumerate() {
        if let Some(targetname) = entity.get("targetname")
            && names
                .iter()
                .any(|name| targetname.eq_ignore_ascii_case(name))
        {
            return Err(format!(
                "Imported filter {targetname} resolves to a BSP entity"
            ));
        }
        let Some(name) = entity
            .get("filtername")
            .and_then(|target| names.iter().find(|name| target.eq_ignore_ascii_case(name)))
        else {
            continue;
        };
        // Source accepts a client touch when the trigger cannot resolve a filter.
        // Only use that behavior for the audited static, unnamed exit boosters.
        let valid_action = if let [(key, value)] = entity.outputs.as_slice() {
            touch_event(key) == Some(false)
                && matches!(
                    parse_player_output(value, false, false, TickInterval::Legacy15Ms.hz())?,
                    Some(PlayerOutput {
                        value: PlayerOutputValue::BaseVelocity(_),
                        ..
                    })
                )
        } else {
            false
        };
        if entity.get("classname") != Some("trigger_multiple")
            || entity
                .get("startdisabled")
                .is_some_and(|value| value != "0")
            || entity
                .get("targetname")
                .is_some_and(|value| !value.is_empty())
            || entity.model_index().is_none_or(|model| model == 0)
            || !static_client_trigger(entity)
            || !valid_action
        {
            return Err(format!(
                "Imported missing filter {name} must be on a static exit booster"
            ));
        }
        unresolved.remove(name.as_str());
        boosters.insert(index);
    }
    if let Some(name) = unresolved.first() {
        return Err(format!(
            "Imported missing filter {name} has no matching exit booster"
        ));
    }
    Ok(boosters)
}

fn add_source_teleports(
    zones: &mut Vec<Zone>,
    geometry: &Geometry,
    entities: &[Entity<'_>],
    timed: bool,
    landmarks: bool,
    extended_names: bool,
) -> Result<(), String> {
    if !timed && !landmarks {
        return Ok(());
    }
    let mut found_landmark = false;
    for entity in entities {
        if entity.get("classname") != Some("trigger_teleport") {
            continue;
        }
        let filter_name = entity.get("filtername").filter(|name| !name.is_empty());
        let landmark_name = entity.get("landmark").filter(|name| !name.is_empty());
        if !(timed && filter_name.is_some() || landmarks && landmark_name.is_some()) {
            continue;
        }
        if extended_names
            && entity.get("model").is_none()
            && static_unnamed_client(entity)
            && entity.outputs.is_empty()
        {
            continue;
        }
        if !static_client_trigger(entity)
            || entity
                .get("startdisabled")
                .is_some_and(|value| value != "0")
            || entity.model_index().is_none_or(|model| model == 0)
            || entity
                .get("targetname")
                .is_some_and(|name| source_target_is_mutated(entities, name))
            || !entity.outputs.is_empty()
            || entity.get("velocitymode").is_some_and(|mode| mode != "0")
            || (!timed && filter_name.is_some())
            || (!landmarks && landmark_name.is_some())
        {
            return Err("Imported Source teleport must be an enabled static client trigger".into());
        }
        let target = entity
            .get("target")
            .ok_or("Imported Source teleport has no destination")?;
        let destination = unique_source_entity(
            entities,
            target,
            &["info_teleport_destination", "info_target"],
            extended_names,
        )?;
        let position = destination
            .origin()
            .ok_or("Imported Source teleport has an invalid destination")?;
        let landmark = match landmark_name {
            Some("!activator") => None,
            Some(name) => Some(
                unique_source_entity(
                    entities,
                    name,
                    &["info_landmark", "info_teleport_destination", "info_target"],
                    extended_names,
                )?
                .origin()
                .ok_or("Imported Source landmark has an invalid origin")?,
            ),
            None => None,
        };
        let view = (landmark_name.is_none() && entity.get("resetang") != Some("0")).then(|| {
            (
                destination.yaw(),
                destination.angles().x.to_radians(),
                f64::from(destination.angles().y),
            )
        });
        let filter = filter_name
            .map(|name| source_activator_filter(entities, name, extended_names))
            .transpose()?;
        let kind = ZoneKind::SourceTeleport(Box::new(SourceTeleport {
            position,
            landmark,
            view,
            filter,
        }));
        let model_index = entity
            .model_index()
            .ok_or("Imported Source teleport has no brush model")?;
        let model = lookup(&geometry.models, model_index, "Source teleport model")?;
        let brushes = geometry.model_brushes(model, entity.placement(), false)?;
        if brushes.is_empty() {
            return Err("Imported Source teleport has no brush".into());
        }
        zones.extend(brushes.into_iter().map(|brush| Zone {
            brush,
            kind: kind.clone(),
        }));
        found_landmark |= landmark_name.is_some();
    }
    if landmarks && !found_landmark {
        return Err("No supported landmark teleports were found in the BSP".into());
    }
    Ok(())
}

fn source_activator_filter(
    entities: &[Entity<'_>],
    name: &str,
    extended_names: bool,
) -> Result<TriggerFilter, String> {
    let filter = unique_source_entity(entities, name, &["filter_activator_name"], extended_names)?;
    let targetname = filter
        .get("filtername")
        .filter(|name| simple_entity_name(name))
        .ok_or("Imported activator filter needs a literal player name")?;
    let negated = match filter.get("negated") {
        None | Some("0" | "Allow entities that match criteria") => false,
        Some("1" | "Disallow entities that match criteria") => true,
        _ => return Err("Imported activator filter has an unsupported negate value".into()),
    };
    if !filter.outputs.is_empty() {
        return Err("Imported activator filter cannot have entity outputs".into());
    }
    Ok(TriggerFilter {
        targetname: targetname.to_ascii_lowercase(),
        negated,
    })
}

fn unique_source_entity<'a, 'b>(
    entities: &'a [Entity<'b>],
    name: &str,
    classes: &[&str],
    extended_names: bool,
) -> Result<&'a Entity<'b>, String> {
    if !literal_target_name(name, extended_names) {
        return Err("Imported Source entity needs a literal target name".into());
    }
    let mut matches = entities.iter().filter(|entity| {
        entity
            .get("targetname")
            .is_some_and(|target| target.eq_ignore_ascii_case(name))
    });
    let entity = matches
        .next()
        .ok_or_else(|| format!("Imported Source target {name} is missing"))?;
    if matches.next().is_some()
        || !entity
            .get("classname")
            .is_some_and(|class| classes.contains(&class))
        || entity
            .get("parentname")
            .is_some_and(|parent| !parent.is_empty())
        || source_target_is_mutated(entities, name)
    {
        return Err(format!(
            "Imported Source target {name} is not a unique static entity"
        ));
    }
    Ok(entity)
}

fn source_target_is_mutated(entities: &[Entity<'_>], name: &str) -> bool {
    !name.is_empty()
        && entities
            .iter()
            .flat_map(|entity| &entity.outputs)
            .any(|(_, action)| {
                let delimiter = if action.contains('\x1b') { '\x1b' } else { ',' };
                let target = action
                    .split(delimiter)
                    .next()
                    .unwrap_or_default()
                    .trim_ascii();
                target.eq_ignore_ascii_case(name)
                    || target.strip_suffix('*').is_some_and(|prefix| {
                        name.get(..prefix.len())
                            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                    })
            })
}

fn touch_event(name: &str) -> Option<bool> {
    if name.eq_ignore_ascii_case("OnStartTouch") {
        Some(true)
    } else if name.eq_ignore_ascii_case("OnEndTouch") {
        Some(false)
    } else {
        None
    }
}

fn source_speedmod_target<'a>(entities: &'a [Entity<'_>]) -> Result<&'a str, String> {
    let mut modifiers = entities
        .iter()
        .filter(|entity| entity.get("classname") == Some("player_speedmod"));
    let target = modifiers
        .next()
        .ok_or("Imported Source speed modifier is missing")?;
    let name = target
        .get("targetname")
        .filter(|name| literal_target_name(name, true))
        .ok_or("Imported Source speed modifier needs a literal target name")?;
    if modifiers.next().is_some()
        || target.get("spawnflags") != Some("4")
        || target
            .get("parentname")
            .is_some_and(|value| !value.is_empty())
        || !target.outputs.is_empty()
        || entities
            .iter()
            .filter(|entity| {
                entity
                    .get("targetname")
                    .is_some_and(|value| value.eq_ignore_ascii_case(name))
            })
            .count()
            != 1
    {
        return Err("Imported Source speed modifier must be a unique jump-only entity".into());
    }
    Ok(name)
}

fn source_speedmod_touch(entity: &Entity<'_>, target: &str) -> Result<Option<f32>, String> {
    let mut start = None;
    let mut end = false;
    for &(event, value) in &entity.outputs {
        let delimiter = if value.contains('\x1b') { '\x1b' } else { ',' };
        let mut parts = value.split(delimiter);
        let name = parts.next().unwrap_or_default().trim_ascii();
        if !name.eq_ignore_ascii_case(target) {
            if name.strip_suffix('*').is_some_and(|prefix| {
                target
                    .get(..prefix.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
            }) {
                return Err("Imported Source speed modifier cannot use a wildcard output".into());
            }
            continue;
        }
        let input = parts.next().unwrap_or_default();
        let scale = parts.next().and_then(|scale| scale.parse::<f32>().ok());
        let Some(scale) = scale.filter(|_| {
            input.eq_ignore_ascii_case("ModifySpeed")
                && parts.next() == Some("0")
                && parts.next() == Some("-1")
                && parts.next().is_none()
        }) else {
            return Err(
                "Imported Source speed modifier needs immediate repeatable touch outputs".into(),
            );
        };
        match touch_event(event) {
            Some(true) if start.is_none() && (0.01..1.0).contains(&scale) => start = Some(scale),
            Some(false) if !end && scale == 1.0 => end = true,
            _ => {
                return Err(
                    "Imported Source speed modifier has an unsupported touch output".into(),
                );
            }
        }
    }
    if start.is_none() && !end {
        return Ok(None);
    }
    if start.is_none()
        || !end
        || entity.outputs.len() != 2
        || entity.get("classname") != Some("trigger_multiple")
        || !matches!(entity.get("spawnflags"), Some("1" | "5"))
        || entity
            .get("parentname")
            .is_some_and(|value| !value.is_empty())
        || entity
            .get("targetname")
            .is_some_and(|value| !value.is_empty())
        || entity
            .get("filtername")
            .is_some_and(|value| !value.is_empty())
        || entity
            .get("startdisabled")
            .is_some_and(|value| value != "0")
        || entity.model_index().is_none_or(|model| model == 0)
    {
        return Err(
            "Imported Source speed modifier requires paired static client touch outputs".into(),
        );
    }
    Ok(start)
}

fn static_client_trigger(entity: &Entity<'_>) -> bool {
    // This subset only models an ordinary player touching a static brush.
    const CLIENTS: u32 = 1;
    const DISALLOW_BOTS: u32 = 4096;
    entity.get("parentname").is_none_or(str::is_empty)
        && entity
            .get("spawnflags")
            .and_then(|flags| flags.parse::<u32>().ok())
            .is_some_and(|flags| flags & !DISALLOW_BOTS == CLIENTS)
}

fn static_unnamed_trigger(entity: &Entity<'_>) -> bool {
    static_unnamed_client(entity) && entity.model_index().is_some_and(|model| model > 0)
}

fn static_unnamed_client(entity: &Entity<'_>) -> bool {
    static_client_trigger(entity)
        && entity.get("targetname").is_none_or(str::is_empty)
        && entity.get("startdisabled").is_none_or(|value| value == "0")
}

fn parse_player_output(
    value: &str,
    gravity_outputs: bool,
    name_outputs: bool,
    tick_hz: f64,
) -> Result<Option<PlayerOutput>, String> {
    let delimiter = if value.contains('\x1b') { '\x1b' } else { ',' };
    let mut parts = value.split(delimiter);
    let target = parts.next().unwrap_or_default().trim_ascii();
    let input = parts.next().unwrap_or_default();
    if !input.eq_ignore_ascii_case("AddOutput") {
        return Ok(None);
    }
    let Some((key, parameter)) = parts.next().and_then(|part| part.split_once(' ')) else {
        return Ok(None);
    };
    let delay = parts.next().unwrap_or_default();
    let count = parts.next().unwrap_or_default();
    let extra = parts.next().is_some();
    if key.eq_ignore_ascii_case("basevelocity") {
        if target.eq_ignore_ascii_case("!activator") && delay == "0" && count == "-1" && !extra {
            return Ok(parse_vector(parameter).map(|velocity| PlayerOutput {
                delay_ticks: 0,
                value: PlayerOutputValue::BaseVelocity(source_axes(velocity)),
            }));
        }
        return Ok(None);
    }
    let gravity = gravity_outputs && key.eq_ignore_ascii_case("gravity");
    let player_name = name_outputs && key.eq_ignore_ascii_case("targetname");
    if !gravity && !player_name {
        return Ok(None);
    }
    if !target.eq_ignore_ascii_case("!activator") || count != "-1" || extra {
        return Err("Imported output must target the activating player and be repeatable".into());
    }
    let value = if gravity {
        PlayerOutputValue::Gravity(parse_gravity(parameter)?)
    } else {
        if !simple_entity_name(parameter) {
            return Err("Invalid imported player target name".into());
        }
        PlayerOutputValue::TargetName(parameter.to_ascii_lowercase())
    };
    Ok(Some(PlayerOutput {
        delay_ticks: output_ticks(delay, tick_hz)?,
        value,
    }))
}

fn output_ticks(seconds: &str, tick_hz: f64) -> Result<u32, String> {
    let seconds = parse_trigger_seconds(seconds)?;
    let ticks = (seconds * tick_hz - 1e-6).ceil();
    if ticks > f64::from(u32::MAX) {
        return Err("Imported trigger timing exceeds the tick range".into());
    }
    Ok(ticks as u32)
}

fn entity_tick_hz(interval: TickInterval) -> f64 {
    match interval {
        TickInterval::Legacy15Ms => interval.hz(),
        // Use the nominal rate for decimal entity delays so exact tick boundaries stay exact.
        TickInterval::Source10Ms => 100.0,
    }
}

fn trigger_wait_ticks(seconds: &str, tick_hz: f64) -> Result<u32, String> {
    let seconds = parse_trigger_seconds(seconds)?;
    let wait = if seconds == 0.0 { 0.2 } else { seconds };
    // Source entity thinks round to a tick. Delayed entity outputs have a separate queue.
    let ticks = (wait * tick_hz).round();
    if ticks > f64::from(u32::MAX) {
        return Err("Imported trigger wait exceeds the tick range".into());
    }
    Ok((ticks as u32).max(1))
}

fn parse_trigger_seconds(value: &str) -> Result<f64, String> {
    value
        .parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .ok_or_else(|| "Invalid imported trigger timing".into())
}

fn simple_entity_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'-'))
}

fn literal_target_name(name: &str, extended_names: bool) -> bool {
    if !extended_names {
        return simple_entity_name(name);
    }
    !name.is_empty()
        && name.len() <= 64
        && name == name.trim_ascii()
        && name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'-' | b'.' | b' '))
}

fn parse_gravity(raw: &str) -> Result<f32, String> {
    raw.trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or_else(|| "Invalid imported gravity scale".into())
}

#[cfg(test)]
mod player_output_tests {
    use super::*;

    #[test]
    fn source_speedmod_requires_static_jump_only_touch_pairs() {
        let modifier =
            r#"{ "classname" "player_speedmod" "targetname" "nojump" "spawnflags" "4" }"#;
        let brush = r#"{
            "classname" "trigger_multiple" "model" "*1" "spawnflags" "5"
            "OnStartTouch" "nojump,ModifySpeed,.9999,0,-1"
            "OnEndTouch" "NoJump,ModifySpeed,1,0,-1"
        }"#;
        let text = format!("{modifier} {brush}");
        let entities = super::super::read_entities(&text).unwrap();
        let target = source_speedmod_target(&entities).unwrap();
        assert_eq!(source_speedmod_touch(&entities[0], target).unwrap(), None);
        assert_eq!(
            source_speedmod_touch(&entities[1], target).unwrap(),
            Some(0.9999)
        );
        for (from, to) in [
            (".9999,0,-1", ".9999,0.1,-1"),
            ("ModifySpeed,1,0,-1", "ModifySpeed,0.9,0,-1"),
            ("\"NoJump,ModifySpeed", "\"noj*,ModifySpeed"),
            (
                "\"model\" \"*1\"",
                "\"model\" \"*1\" \"parentname\" \"mover\"",
            ),
            ("\"spawnflags\" \"5\"", "\"spawnflags\" \"4\""),
        ] {
            let invalid = brush.replace(from, to);
            let entities = super::super::read_entities(&invalid).unwrap();
            assert!(source_speedmod_touch(&entities[0], target).is_err());
        }
        let invalid = text.replace("\"spawnflags\" \"4\"", "\"spawnflags\" \"12\"");
        assert!(source_speedmod_target(&super::super::read_entities(&invalid).unwrap()).is_err());
    }
    use crate::source_bsp::read_entities;

    #[test]
    fn source_boosters_accept_case_insensitive_base_velocity_keys() {
        let tick_hz = TickInterval::Legacy15Ms.hz();
        for action in [
            "!activator,AddOutput,Basevelocity 2 3 400,0,-1",
            "!activator,AddOutput,basevelocity 2 3 400,0,-1",
            "!ACTIVATOR,addoutput,BASEVELOCITY 2 3 400,0,-1",
            "!activator\x1bAddOutput\x1bbasevelocity 2 3 400\x1b0\x1b-1",
        ] {
            let output = parse_player_output(action, false, false, tick_hz)
                .unwrap()
                .unwrap();
            assert_eq!(output.delay_ticks, 0);
            assert!(matches!(
                output.value,
                PlayerOutputValue::BaseVelocity(value) if value == Vec3::new(2.0, 400.0, -3.0)
            ));
        }
        for action in [
            "!activator,AddOutput,basevelocity 0 0 400,0.2,-1",
            "other,AddOutput,basevelocity 0 0 400,0,-1",
            "!activator,AddOutput,basevelocity 0 0 400,0,1",
        ] {
            assert!(
                parse_player_output(action, false, false, tick_hz)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn source_gravity_outputs_are_opt_in_and_delays_use_fixed_ticks() {
        let tick_hz = TickInterval::Legacy15Ms.hz();
        let lift = "!activator ,AddOutput,gravity -0.6,0,-1";
        assert!(
            parse_player_output(lift, false, false, tick_hz)
                .unwrap()
                .is_none()
        );
        let output = parse_player_output(lift, true, false, tick_hz)
            .unwrap()
            .unwrap();
        assert_eq!(output.delay_ticks, 0);
        assert!(matches!(output.value, PlayerOutputValue::Gravity(-0.6)));

        let reset = parse_player_output(
            "!activator,AddOutput,gravity 1,0.5,-1",
            true,
            false,
            tick_hz,
        )
        .unwrap()
        .unwrap();
        assert_eq!(reset.delay_ticks, 34);
        assert!(matches!(reset.value, PlayerOutputValue::Gravity(1.0)));
        for action in [
            "!activator,AddOutput,gravity -1,0,1",
            "!activator,AddOutput,gravity -1,-0.1,-1",
            "!activator,AddOutput,gravity NaN,0,-1",
            "other,AddOutput,gravity -1,0,-1",
        ] {
            assert!(parse_player_output(action, true, false, tick_hz).is_err());
        }
    }

    #[test]
    fn source_timed_triggers_round_wait_and_dispatch_delays_independently() {
        let tick_hz = TickInterval::Legacy15Ms.hz();
        assert_eq!(trigger_wait_ticks("0", tick_hz).unwrap(), 13);
        assert_eq!(trigger_wait_ticks("0.01", tick_hz).unwrap(), 1);
        assert_eq!(trigger_wait_ticks("0.1", tick_hz).unwrap(), 7);
        assert_eq!(trigger_wait_ticks("1", tick_hz).unwrap(), 67);
        assert_eq!(output_ticks("0.09", tick_hz).unwrap(), 6);
        assert_eq!(output_ticks("0.1", tick_hz).unwrap(), 7);
        let action = parse_player_output(
            "!activator,AddOutput,targetname Active,0.09,-1",
            false,
            true,
            tick_hz,
        )
        .unwrap()
        .unwrap();
        assert_eq!(action.delay_ticks, 6);
        assert!(matches!(action.value, PlayerOutputValue::TargetName(name) if name == "active"));

        let tick_hz = entity_tick_hz(TickInterval::Source10Ms);
        assert_eq!(trigger_wait_ticks("0", tick_hz).unwrap(), 20);
        assert_eq!(trigger_wait_ticks("0.012", tick_hz).unwrap(), 1);
        assert_eq!(output_ticks("0.012", tick_hz).unwrap(), 2);
        assert_eq!(output_ticks("1", tick_hz).unwrap(), 100);
        assert_eq!(output_ticks("0.1", tick_hz).unwrap(), 10);
    }

    #[test]
    fn source_rules_four_resolve_literal_destination_names_without_selectors() {
        let entities = read_entities(
            r#"
            { "classname" "info_teleport_destination" "targetname" "Stage 2 Easy" }
            { "classname" "info_teleport_destination" "targetname" "s2_3.5" }
        "#,
        )
        .unwrap();
        let resolve = |name, extended| {
            unique_source_entity(&entities, name, &["info_teleport_destination"], extended)
        };
        assert!(resolve("stage 2 easy", false).is_err());
        assert!(resolve("stage 2 easy", true).is_ok());
        assert!(resolve("s2_3.5", true).is_ok());
        assert!(resolve("Stage*", true).is_err());
        assert!(resolve("!activator", true).is_err());
        assert!(resolve(" Stage 2 Easy", true).is_err());
    }

    #[test]
    fn source_missing_filter_rules_accept_only_unresolved_static_exit_boosters() {
        let booster = r#"{
            "classname" "trigger_multiple"
            "filtername" "fix"
            "model" "*1"
            "spawnflags" "4097"
            "StartDisabled" "0"
            "OnEndTouch" "!activator,AddOutput,basevelocity 0 0 450,0,-1"
        }"#;
        let filters = ["fix".into()];
        let entities = read_entities(booster).unwrap();
        assert!(missing_filter_boosters(&[], &entities).unwrap().is_empty());
        assert_eq!(
            missing_filter_boosters(&filters, &entities).unwrap(),
            BTreeSet::from([0])
        );
        assert!(missing_filter_boosters(&["other".into()], &entities).is_err());
        for invalid in [
            booster.replace("\"OnEndTouch\"", "\"OnStartTouch\""),
            booster.replace("\"trigger_multiple\"", "\"trigger_push\""),
            booster.replace("\"StartDisabled\" \"0\"", "\"StartDisabled\" \"1\""),
            booster.replace("\"model\" \"*1\"", "\"model\" \"*0\""),
            booster.replace(
                "\"model\" \"*1\"",
                "\"model\" \"*1\" \"targetname\" \"dynamic\"",
            ),
            format!("{booster} {{ \"classname\" \"info_target\" \"targetname\" \"FIX\" }}"),
        ] {
            assert!(missing_filter_boosters(&filters, &read_entities(&invalid).unwrap()).is_err());
        }
        assert!(!valid_source_missing_filters(&["fix".into(), "fix".into()]));
        assert!(!valid_source_missing_filters(&["fix*".into()]));
    }
}
