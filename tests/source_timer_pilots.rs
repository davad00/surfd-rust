use std::path::PathBuf;

use bevy_surf::{
    catalog::{self, MapMode},
    course::{Course, Zone, ZoneKind},
    movement::{MoveInput, Player},
    replay::TickCommand,
    session::{RunPhase, Session},
    source_bsp,
};

#[test]
#[ignore = "requires the exact Steam Workshop surf_aweles BSP and its timer sidecar"]
fn aweles_authored_stage_drops() {
    let root = PathBuf::from(std::env::var_os("SURF_PILOT_MAPS").expect("Set SURF_PILOT_MAPS"));
    let path = root.join("surf_aweles.bsp");
    let catalog =
        catalog::parse_source_catalog(include_bytes!("../assets/practice-source-catalog.json"))
            .unwrap();
    let entry = catalog
        .maps
        .iter()
        .find(|entry| entry.id == "surf_aweles")
        .unwrap();
    let mut old_rules = entry.clone();
    old_rules.min_source_rules -= 1;
    assert!(source_bsp::load_ranked(&path, &old_rules).is_err());
    let course = source_bsp::load_ranked(&path, entry).unwrap();
    assert_eq!(course.practice_spawns.len(), 4);
    assert_eq!(Session::new(&course).checkpoint_count(), 4);
    let mut session = walk_out_of_start(&course, None).expect("Aweles could not start a run");
    let mut practice = Session::new(&course);
    for (index, (x, y, z, yaw)) in [
        (13390.7, 2440.0, -1154.0, 90.5),
        (10083.0, 2813.0, -9716.0, 92.0),
        (5647.0, 386.0, -5605.0, 269.0),
        (4043.0, -520.0, -7317.0, 266.0),
        (-519.0, 553.0, -11135.0, 268.5),
    ]
    .into_iter()
    .enumerate()
    {
        let stage = index + 1;
        let start = course
            .zones
            .iter()
            .find(|zone| match zone.kind {
                ZoneKind::Start => stage == 1,
                ZoneKind::StageStart(number) => usize::from(number) == stage,
                _ => false,
            })
            .unwrap();
        if stage > 1 {
            session.player = Player::new((start.brush.min + start.brush.max) * 0.5);
            session.tick(&course, MoveInput::default());
        }
        assert_eq!(session.stage_progress(&course), Some((stage, 5)));
        let splits = session.splits.clone();
        let elapsed = session.elapsed;
        let restart_yaw = session
            .tick_command(
                &course,
                TickCommand {
                    input: MoveInput::default(),
                    pitch: 0.0,
                    restart_stage: true,
                    source_yaw_degrees: None,
                },
            )
            .unwrap();
        assert_eq!(session.player.position.to_array(), [x, y, z]);
        assert_eq!(session.source_yaw_override(), Some(yaw));
        let grounded_tick = settle_aweles(&course, &mut session, restart_yaw, start);
        assert_eq!(session.phase, RunPhase::Running);
        assert_eq!(session.stage_progress(&course), Some((stage, 5)));
        assert_eq!(session.splits, splits);
        assert!(session.elapsed > elapsed);
        let selection = index.checked_sub(1).map(|i| &course.practice_spawns[i]);
        practice.set_practice(&course, selection);
        assert_eq!(practice.player.position.to_array(), [x, y, z]);
        assert_eq!(practice.stage_progress(&course), Some((stage, 5)));
        let practice_tick = settle_aweles(&course, &mut practice, restart_yaw, start);
        assert_eq!(practice.stage_progress(&course), Some((stage, 5)));
        assert_eq!(
            practice.phase,
            if stage == 1 {
                RunPhase::Ready
            } else {
                RunPhase::Untimed
            }
        );
        let launch_tick = launch_aweles(&course, &mut practice, restart_yaw, start);
        assert_eq!(practice.stage_progress(&course), Some((stage, 5)));
        assert_eq!(
            practice.phase,
            if stage == 1 {
                RunPhase::Running
            } else {
                RunPhase::Untimed
            }
        );
        eprintln!(
            "Aweles stage {stage} grounded in its timer region on restart tick {grounded_tick} and practice tick {practice_tick}, then launched after {launch_tick} forward ticks"
        );
    }
}

fn settle_aweles(course: &Course, session: &mut Session, yaw: f32, start: &Zone) -> usize {
    let revision = session.pose_revision();
    for tick in 1..=67 {
        session.tick(
            course,
            MoveInput {
                yaw,
                ..Default::default()
            },
        );
        assert_eq!(session.pose_revision(), revision);
        if session.player.grounded {
            let (min, max) = session.player.hull_bounds();
            assert!(min.cmple(start.brush.max).all());
            assert!(max.cmpge(start.brush.min).all());
            return tick;
        }
    }
    panic!("Aweles start did not settle");
}

fn launch_aweles(course: &Course, session: &mut Session, yaw: f32, start: &Zone) -> usize {
    let revision = session.pose_revision();
    for tick in 1..=180 {
        session.tick(
            course,
            MoveInput {
                forward: 1.0,
                yaw,
                ..Default::default()
            },
        );
        assert_eq!(session.pose_revision(), revision);
        let (min, max) = session.player.hull_bounds();
        if !min.cmple(start.brush.max).all() || !max.cmpge(start.brush.min).all() {
            let velocity = session.player.velocity;
            assert!(velocity.x.hypot(velocity.z) > 100.0);
            return tick;
        }
    }
    panic!("Aweles could not launch from its authored start");
}

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
