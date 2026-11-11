use bevy_surf::{
    course::Course,
    movement::MoveInput,
    session::{RunPhase, Session},
};

#[test]
fn downline_can_be_surfed_from_spawn_to_finish() {
    let course = Course::practice();
    let mut session = Session::new(&course);
    assert_eq!(session.phase, RunPhase::Ready);

    let mut started = false;
    let mut top_speed = 0.0_f32;
    for _ in 0..600 {
        let position = session.player.position;
        let velocity = session.player.velocity;
        let mut input = MoveInput {
            yaw: course.spawn_yaw,
            ..Default::default()
        };
        // Run off the starting deck. On the right face, strafe toward the ridge
        // when we are drifting too far or too fast toward the outside edge.
        if position.z > -400.0 || position.z < -2100.0 {
            input.forward = 1.0;
        } else if position.x > 120.0 || velocity.x > 28.0 {
            input.strafe = -1.0;
        }

        session.tick(&course, input);
        started |= session.phase == RunPhase::Running;
        top_speed = top_speed.max(-session.player.velocity.z);
        if session.phase == RunPhase::Finished {
            break;
        }
    }

    assert!(started);
    assert!(top_speed > 700.0, "top downcourse speed was {top_speed}");
    assert_eq!(
        session.phase,
        RunPhase::Finished,
        "last player position was {:?}",
        session.player.position
    );
    let [split] = session.splits.as_slice() else {
        panic!("expected a checkpoint split, got {:?}", session.splits);
    };
    assert!(*split > 0.0 && *split < session.elapsed);
    assert_eq!(session.best, Some(session.elapsed));
}
