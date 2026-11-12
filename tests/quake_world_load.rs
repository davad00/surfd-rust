use std::path::PathBuf;

use bevy_surf::{
    catalog::MapMode,
    course::{Course, Zone, ZoneKind},
    local_map,
    movement::{MoveInput, Player},
    session::{RunPhase, Session},
};

#[test]
#[ignore = "requires local QuakeWorld BSPs and the official KTX race routes"]
fn quakeworld_maps_load_playable_race_routes() {
    let root = PathBuf::from(std::env::var_os("SURF_QUAKE_PILOTS").expect("Set SURF_QUAKE_PILOTS"));
    for (name, mode, checkpoints, bonuses) in [
        ("dungeonsurf", MapMode::Surf, 1, 1),
        ("rampcity", MapMode::Surf, 0, 1),
        ("hoppa2", MapMode::Bhop, 0, 0),
        ("ztricks", MapMode::Bhop, 7, 9),
    ] {
        let course = local_map::load(&root.join(format!("{name}.bsp")))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let mut session = Session::new(&course);
        assert_eq!(course.map_mode(), mode, "{name}");
        assert_eq!(session.phase, RunPhase::Ready, "{name}");
        assert_eq!(session.checkpoint_count(), checkpoints, "{name}");
        assert_eq!(course.bonus_routes.len(), bonuses, "{name}");
        assert!(!session.ranked_eligible(), "{name}");
        assert!(course.brushes.len() > 20, "{name}");
        assert!(course.surfaces.len() > 100, "{name}");
        assert!(!course.textures.is_empty(), "{name}");
        let mut run =
            walk_out_of_start(&course).unwrap_or_else(|| panic!("{name} could not start a run"));
        let finish = course.zones.last().unwrap();
        assert!(matches!(finish.kind, ZoneKind::Finish), "{name}");
        if checkpoints > 0 {
            touch_zone(&course, &mut run, finish);
            assert_eq!(run.phase, RunPhase::Running, "{name} skipped checkpoints");
        }
        for order in 1..=checkpoints {
            let zone = course.zones.iter().find(|zone| {
                matches!(zone.kind, ZoneKind::Checkpoint(index) if usize::from(index) == order)
            }).unwrap();
            touch_zone(&course, &mut run, zone);
            assert_eq!(run.splits.len(), order, "{name} checkpoint {order}");
        }
        touch_zone(&course, &mut run, finish);
        assert_eq!(run.phase, RunPhase::Finished, "{name}");
        assert!(run.elapsed > 0.0, "{name}");
        for route in &course.bonus_routes {
            session.select_bonus(&course, Some(route.number)).unwrap();
            assert_eq!(
                session.phase,
                RunPhase::Ready,
                "{name} bonus {}",
                route.number
            );
            assert_eq!(
                session.player.position, route.spawn.position,
                "{name} bonus {}",
                route.number
            );
        }
    }
}

fn touch_zone(course: &Course, session: &mut Session, zone: &Zone) {
    session.player = Player::new((zone.brush.min + zone.brush.max) * 0.5);
    session.tick(course, MoveInput::default());
}

fn walk_out_of_start(course: &Course) -> Option<Session> {
    for direction in 0..8 {
        let mut session = Session::new(course);
        for _ in 0..90 {
            session.tick(course, MoveInput::default());
        }
        let input = MoveInput {
            forward: 1.0,
            yaw: direction as f32 * std::f32::consts::FRAC_PI_4,
            ..MoveInput::default()
        };
        for _ in 0..180 {
            session.tick(course, input);
            if session.phase == RunPhase::Running {
                return Some(session);
            }
        }
    }
    None
}

#[test]
#[ignore = "requires local GoldSrc surf map samples"]
fn goldsrc_surf_maps_load_collision_and_visuals() {
    let root =
        PathBuf::from(std::env::var_os("SURF_LEGACY_PILOTS").expect("Set SURF_LEGACY_PILOTS"));
    for name in ["surf_the-gap", "surf_speedway_b2"] {
        let course = local_map::load(&root.join(format!("{name}.bsp")))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(course.map_mode(), MapMode::Surf, "{name}");
        assert!(course.brushes.len() > 20, "{name}");
        assert!(course.surfaces.len() > 100, "{name}");
        assert!(
            course
                .textures
                .iter()
                .any(|texture| !texture.rgba.is_empty()),
            "{name}"
        );
        let mut session = Session::new(&course);
        assert!(!session.ranked_eligible(), "{name}");
        for _ in 0..45 {
            session.tick(&course, MoveInput::default());
        }
        let at_rest = session.player.position;
        for _ in 0..24 {
            session.tick(
                &course,
                MoveInput {
                    forward: 1.0,
                    yaw: course.spawn_yaw,
                    ..Default::default()
                },
            );
        }
        let delta = session.player.position - at_rest;
        assert!(
            delta.x.hypot(delta.z) > 2.0,
            "{name} spawn {at_rest:?}, delta {delta:?}"
        );
    }
}
