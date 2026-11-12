use std::path::PathBuf;

use bevy_surf::{
    catalog::MapMode,
    course::{Course, Zone, ZoneKind},
    movement::{MoveInput, Player},
    session::{RunPhase, Session},
    surfd_map,
};

#[test]
#[ignore = "requires the local Steam Workshop surf_beginner Source 2 conversion"]
fn cs2_surf_beginner_runs_its_authored_timer() {
    let root =
        PathBuf::from(std::env::var_os("SURF_SOURCE2_PILOT").expect("Set SURF_SOURCE2_PILOT"));
    let course = surfd_map::load(
        &root.join("surf_beginner_cs2.geometry.json.gz"),
        Some(&root.join("surf_beginner_cs2.visual.json.gz")),
    )
    .unwrap();
    assert_eq!(course.map_mode(), MapMode::Surf);
    assert!(course.brushes.len() > 2_000);
    assert!(course.visual_meshes.len() > 200);
    assert!(course.textures.len() >= 10);
    assert_eq!(course.practice_spawns.len(), 7);

    let initial = Session::new(&course);
    assert_eq!(initial.phase, RunPhase::Ready);
    assert_eq!(initial.checkpoint_count(), 6);
    assert!(!initial.ranked_eligible());
    let finish = course
        .zones
        .iter()
        .find(|zone| matches!(zone.kind, ZoneKind::Finish))
        .unwrap();
    let mut early_finish = running_session(&course);
    touch(&mut early_finish, &course, finish);
    assert_eq!(early_finish.phase, RunPhase::Running);

    let mut session = running_session(&course);
    for order in 1..=6 {
        let stage = &course.practice_spawns[order];
        session.player = Player::new(stage.position);
        session.tick(&course, MoveInput::default());
        assert_eq!(
            session.splits.len(),
            order,
            "checkpoint {order}, {:?}, {:?}",
            session.phase,
            session.player.position
        );
    }
    touch(&mut session, &course, finish);
    assert_eq!(session.phase, RunPhase::Finished);
    assert!(session.elapsed > 0.0);
}

fn running_session(course: &Course) -> Session {
    let mut session = Session::new(course);
    for _ in 0..40 {
        session.tick(course, MoveInput::default());
    }
    let rest = session.player.position;
    for _ in 0..150 {
        session.tick(
            course,
            MoveInput {
                forward: 1.0,
                yaw: course.spawn_yaw,
                ..Default::default()
            },
        );
        if session.phase == RunPhase::Running {
            break;
        }
    }
    assert_eq!(
        session.phase,
        RunPhase::Running,
        "rest {rest:?}, player {:?}",
        session.player.position
    );
    session
}

fn touch(session: &mut Session, course: &Course, zone: &Zone) {
    session.player = Player::new((zone.brush.min + zone.brush.max) * 0.5);
    session.tick(course, MoveInput::default());
}
