use std::path::PathBuf;

use bevy_surf::{
    catalog::{self, MapMode},
    course::{Course, ZoneKind},
    movement::{MoveInput, Player},
    session::{RunPhase, Session},
    source_bsp,
};

#[test]
#[ignore = "requires the local bhop_easy BSP and pinned timer file"]
fn ranked_source_uses_the_pinned_bhop_board() {
    let root = PathBuf::from(std::env::var_os("SURF_PILOT_MAPS").expect("Set SURF_PILOT_MAPS"));
    let path = root.join("bhop_easy.bsp");
    let entry = catalog::find("bhop_easy").unwrap();
    let course = source_bsp::load_ranked(&path, entry).unwrap();
    let source = entry.source_bsp.as_ref().unwrap();
    assert_eq!(course.map_mode(), MapMode::Bhop);
    assert_eq!(course.board_revision(None), Some(source.board_revision()));
    assert_eq!(
        course.board_revision(Some(1)),
        source.bonus_board_revision(1)
    );
    assert!(Session::new(&course).ranked_eligible());
    let mut modified = entry.clone();
    modified.source_bsp.as_mut().unwrap().timer_sha256 = "00".repeat(32);
    assert!(source_bsp::load_ranked(&path, &modified).is_err());
}

#[test]
#[ignore = "requires the downloaded Strata version 25 samples"]
fn strata_source_maps_load_render_and_collision_data() {
    let root = PathBuf::from(std::env::var_os("SURF_STRATA_MAPS").expect("Set SURF_STRATA_MAPS"));
    for (name, mode) in [
        ("surf_blink", MapMode::Surf),
        ("surf_skyhaven", MapMode::Surf),
    ] {
        let course = source_bsp::load(&root.join(format!("{name}.bsp"))).unwrap();
        assert_eq!(course.map_mode(), mode);
        assert!(
            course.brushes.len() > 20 && course.surfaces.len() > 100,
            "{name}"
        );
    }
}

#[test]
#[ignore = "requires the three local Source BSP and timer files"]
fn pilot_routes_are_timed_locally() {
    let root = PathBuf::from(std::env::var_os("SURF_PILOT_MAPS").expect("Set SURF_PILOT_MAPS"));
    for (name, splits, stages, bonuses) in [
        ("surf_1998", 4, 4, 1),
        ("bhop_easy", 0, 0, 1),
        ("surf_aspect", 2, 0, 2),
    ] {
        let course = source_bsp::load(&root.join(format!("{name}.bsp"))).unwrap();
        let mut session = Session::new(&course);
        assert_eq!(session.phase, RunPhase::Ready, "{name}");
        assert_eq!(session.checkpoint_count(), splits, "{name}");
        assert_eq!(course.practice_spawns.len(), stages, "{name}");
        assert_eq!(course.bonus_routes.len(), bonuses, "{name}");
        assert!(!session.ranked_eligible(), "{name}");
        let mut routes = vec![(None, course.spawn, course.zones.as_slice())];
        routes.extend(course.bonus_routes.iter().map(|route| {
            (
                Some(route.number),
                route.spawn.position,
                route.zones.as_slice(),
            )
        }));
        for (number, spawn, zones) in routes {
            session.select_bonus(&course, number).unwrap();
            assert_eq!(session.phase, RunPhase::Ready, "{name} {number:?}");
            assert_eq!(session.player.position, spawn, "{name} {number:?}");
            assert!(!session.ranked_eligible(), "{name} {number:?}");
            assert!(
                zones.iter().any(|zone| matches!(zone.kind, ZoneKind::Start)
                    && zone.brush.min.x <= spawn.x
                    && spawn.x <= zone.brush.max.x
                    && zone.brush.min.z <= spawn.z
                    && spawn.z <= zone.brush.max.z
                    && (zone.brush.min.y - 1.0..=zone.brush.max.y + 32.0).contains(&spawn.y)),
                "{name} {number:?} {spawn:?}"
            );
            assert!(
                zones.iter().any(|zone| matches!(
                    zone.kind,
                    ZoneKind::Teleport { .. }
                        | ZoneKind::AuthoredTeleport(_)
                        | ZoneKind::MomentumTeleport { .. }
                )),
                "{name} {number:?}"
            );
            let mut session = walk_out_of_start(&course, number)
                .unwrap_or_else(|| panic!("{name} {number:?} could not start a timed run"));
            if name == "surf_1998" && number.is_none() {
                session.restart_stage(&course).unwrap();
                assert_eq!(session.player.position, course.spawn);
                assert_eq!(session.phase, RunPhase::Running);
            }
            let touch = |session: &mut Session, kind: fn(&ZoneKind) -> bool| {
                let zone = zones.iter().find(|zone| kind(&zone.kind)).unwrap();
                session.player = Player::new((zone.brush.min + zone.brush.max) * 0.5);
                session.tick(&course, MoveInput::default());
            };
            let finish = |kind: &ZoneKind| matches!(kind, ZoneKind::Finish);
            if number.is_none() && splits > 0 {
                touch(&mut session, finish);
                assert_eq!(
                    session.phase,
                    RunPhase::Running,
                    "{name} let the player skip checkpoints"
                );
                for order in 1..=splits {
                    let zone = zones.iter().find(|zone| matches!(zone.kind, ZoneKind::Checkpoint(index) if usize::from(index) == order)).unwrap();
                    session.player = Player::new((zone.brush.min + zone.brush.max) * 0.5);
                    session.tick(&course, MoveInput::default());
                    assert_eq!(session.splits.len(), order, "{name} checkpoint {order}");
                }
            }
            touch(&mut session, finish);
            assert_eq!(
                session.phase,
                RunPhase::Finished,
                "{name} {number:?} finish"
            );
            assert!(session.elapsed > 0.0, "{name} {number:?}");
        }
    }
}

fn walk_out_of_start(course: &Course, bonus: Option<u16>) -> Option<Session> {
    for direction in 0..8 {
        let mut session = Session::new(course);
        session.select_bonus(course, bonus).unwrap();
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
