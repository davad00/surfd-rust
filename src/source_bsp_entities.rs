use std::ops::Range;

use bevy_math::Vec3;

use crate::course::{Conveyor, ZoneKind};

use super::{Entity, parse_vector, source_axes};

const TRIGGER_CLIENTS: u32 = 1;
const TELEPORT_PRESERVE_ANGLES: u32 = 0x20;
const PUSH_ONCE: u32 = 128;
const DOOR_TOUCH_OPENS: u32 = 1024;
const CONVEYOR_VISUAL_ONLY: u32 = 1;
const CONVEYOR_NOT_SOLID: u32 = 2;

pub(super) struct BrushSurfaceKind {
    pub(super) solid: bool,
}

pub(super) fn brush_surface_kind(
    entity: &Entity<'_>,
    allow_static_bhop_platforms: bool,
) -> Option<BrushSurfaceKind> {
    let solid = match entity.get("classname")? {
        "func_brush" | "func_wall" | "func_lod" => entity.get("solidity") != Some("1"),
        "func_illusionary" => false,
        "func_conveyor" => spawn_flags(entity) & CONVEYOR_NOT_SOLID == 0,
        "func_door" if allow_static_bhop_platforms && is_bhop_platform(entity) => true,
        _ => return None,
    };
    Some(BrushSurfaceKind { solid })
}

fn is_bhop_platform(entity: &Entity<'_>) -> bool {
    spawn_flags(entity) & DOOR_TOUCH_OPENS != 0
        && entity
            .get("spawnpos")
            .is_none_or(|position| position == "0")
        && entity.get("targetname").is_none_or(str::is_empty)
        && direction(entity, "movedir").is_some_and(|direction| direction.y < -0.9999)
}

pub(super) fn conveyor(entity: &Entity<'_>, brush_indices: Range<usize>) -> Option<Conveyor> {
    if entity.get("classname") != Some("func_conveyor")
        || spawn_flags(entity) & CONVEYOR_VISUAL_ONLY != 0
        || brush_indices.is_empty()
    {
        return None;
    }
    Some(Conveyor {
        brush_indices: brush_indices.collect(),
        velocity: velocity(entity, "movedir")?,
    })
}

pub(super) fn trigger(
    entity: &Entity<'_>,
    entities: &[Entity<'_>],
    push_index: usize,
    has_timer: bool,
) -> Option<ZoneKind> {
    let class = entity.get("classname")?;
    if !class.starts_with("trigger_") {
        return None;
    }
    if !has_timer {
        let name = entity
            .get("targetname")
            .unwrap_or_default()
            .to_ascii_lowercase();
        match name.as_str() {
            "surf_start" | "map_start" => return Some(ZoneKind::Start),
            "surf_finish" | "surf_end" | "map_end" => return Some(ZoneKind::Finish),
            _ => {}
        }
        if let Some(index) = name
            .strip_prefix("surf_checkpoint_")
            .and_then(|value| value.parse().ok())
        {
            return Some(ZoneKind::Checkpoint(index));
        }
    }

    if class == "trigger_hurt" && entity.get("damage")?.parse::<f32>().ok()? >= 100.0 {
        return Some(ZoneKind::Reset);
    }
    if entity
        .get("filtername")
        .is_some_and(|name| !name.is_empty())
    {
        return None;
    }
    match class {
        "trigger_push"
            if spawn_flags(entity) & (TRIGGER_CLIENTS | PUSH_ONCE) == TRIGGER_CLIENTS =>
        {
            Some(ZoneKind::Push {
                index: push_index,
                velocity: velocity(entity, "pushdir")?,
            })
        }
        "trigger_teleport" => teleport(entity, entities),
        _ => None,
    }
}

fn teleport(entity: &Entity<'_>, entities: &[Entity<'_>]) -> Option<ZoneKind> {
    if entity.get("landmark").is_some_and(|name| !name.is_empty()) {
        return None;
    }
    let target = entity.get("target")?;
    let destination = entities.iter().find(|entity| {
        entity
            .get("targetname")
            .is_some_and(|name| name.eq_ignore_ascii_case(target))
    })?;
    let position = destination.origin()?;
    let yaw = (spawn_flags(entity) & TELEPORT_PRESERVE_ANGLES == 0
        && entity.get("resetang") != Some("0"))
    .then(|| destination.yaw());

    // Momentum ports mark fail teleports with an explicit velocity mode.
    match entity.get("velocitymode") {
        None | Some("0") => Some(ZoneKind::Teleport { position, yaw }),
        Some(mode @ ("1" | "2")) => Some(ZoneKind::MomentumTeleport {
            position,
            yaw,
            keep_downward_velocity: mode == "2",
        }),
        _ => None,
    }
}

fn spawn_flags(entity: &Entity<'_>) -> u32 {
    entity
        .get("spawnflags")
        .and_then(|value| value.parse().ok())
        .unwrap_or_default()
}

fn velocity(entity: &Entity<'_>, direction_key: &str) -> Option<Vec3> {
    let speed = entity.get("speed").unwrap_or("0").parse::<f32>().ok()?;
    if !speed.is_finite() {
        return None;
    }
    let speed = if speed == 0.0 { 100.0 } else { speed };
    Some(direction(entity, direction_key)? * speed)
}

fn direction(entity: &Entity<'_>, key: &str) -> Option<Vec3> {
    let angles = parse_vector(entity.get(key).unwrap_or("0 0 0"))?;
    let (sin_pitch, cos_pitch) = angles.x.to_radians().sin_cos();
    let (sin_yaw, cos_yaw) = angles.y.to_radians().sin_cos();
    Some(source_axes(Vec3::new(
        cos_pitch * cos_yaw,
        cos_pitch * sin_yaw,
        -sin_pitch,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity<'a>(pairs: &[(&str, &'a str)]) -> Entity<'a> {
        Entity {
            values: pairs
                .iter()
                .map(|&(key, value)| (key.into(), value))
                .collect(),
            outputs: Vec::new(),
        }
    }

    #[test]
    fn touch_open_bhop_platforms_use_closed_geometry_only_when_enabled() {
        let platform = entity(&[
            ("classname", "func_door"),
            ("spawnflags", "1024"),
            ("movedir", "90 0 0"),
            ("spawnpos", "0"),
        ]);
        assert!(brush_surface_kind(&platform, false).is_none());
        assert!(brush_surface_kind(&platform, true).unwrap().solid);
        let regular_door = entity(&[("classname", "func_door"), ("spawnflags", "1024")]);
        assert!(brush_surface_kind(&regular_door, true).is_none());
    }

    #[test]
    fn imported_push_and_conveyor_use_source_angles() {
        let push = entity(&[
            ("classname", "trigger_push"),
            ("spawnflags", "1"),
            ("pushdir", "-90 0 0"),
            ("speed", "500"),
        ]);
        let Some(ZoneKind::Push { index, velocity }) = trigger(&push, &[], 12, true) else {
            panic!("expected a player push");
        };
        assert_eq!(index, 12);
        assert!(velocity.abs_diff_eq(Vec3::Y * 500.0, 0.001));
        let belt = entity(&[("classname", "func_conveyor"), ("speed", "1800")]);
        let belt = conveyor(&belt, 2..4).unwrap();
        assert_eq!(belt.velocity, Vec3::X * 1800.0);
        assert_eq!(belt.brush_indices, vec![2, 3]);
    }

    #[test]
    fn momentum_teleport_retains_only_downward_velocity() {
        let source = entity(&[
            ("classname", "trigger_teleport"),
            ("target", "Start"),
            ("spawnflags", "4097"),
            ("velocitymode", "2"),
        ]);
        let destination = entity(&[("targetname", "start"), ("origin", "10 20 30")]);
        let Some(ZoneKind::MomentumTeleport {
            position,
            yaw,
            keep_downward_velocity: true,
        }) = trigger(&source, &[destination], 0, true)
        else {
            panic!("expected a Momentum teleport");
        };
        assert_eq!(position, Vec3::new(10.0, 30.0, -20.0));
        assert_eq!(yaw, Some(-std::f32::consts::FRAC_PI_2));
    }
}
