use bevy_surf::{
    course::Course,
    movement::MoveInput,
    replay::Playback,
    session::{RunPhase, Session},
    tutorial,
};

#[test]
fn movement_lesson_requires_a_turn_and_supports_a_running_jump() {
    let course = tutorial::course("tutorial-movement").unwrap();
    let mut session = Session::new(&course);
    for _ in 0..300 {
        session.tick(
            &course,
            MoveInput {
                forward: 1.0,
                ..Default::default()
            },
        );
    }
    assert_ne!(session.phase, RunPhase::Finished);
    session.restart(&course);

    let walk_around_corner = |session: &Session| {
        let position = session.player.position;
        let turned = position.z < -352.0 || position.x > 24.0;
        MoveInput {
            forward: 1.0,
            yaw: if turned {
                -std::f32::consts::FRAC_PI_2
            } else {
                0.0
            },
            ..Default::default()
        }
    };
    for _ in 0..360 {
        session.tick(&course, walk_around_corner(&session));
    }
    assert_ne!(session.phase, RunPhase::Finished);
    let mut saw_jump = false;
    complete_demo(&course, |playback| {
        saw_jump |= playback.displayed_input().jump
    });
    assert!(saw_jump);
}

#[test]
fn catching_and_riding_the_right_face_reaches_the_landing() {
    let course = tutorial::course("tutorial-catch").unwrap();
    let mut top_speed = 0.0_f32;
    complete_demo(&course, |playback| {
        top_speed = top_speed.max(-playback.session.player.velocity.z);
    });
    assert!(top_speed > 500.0);
}

#[test]
fn coordinated_air_strafe_reaches_the_offset_landing() {
    let course = tutorial::course("tutorial-strafe").unwrap();
    let mut session = Session::new(&course);
    for _ in 0..380 {
        let input = if session.player.position.y > 517.0 {
            MoveInput {
                forward: 1.0,
                ..Default::default()
            }
        } else {
            MoveInput {
                strafe: 1.0,
                ..Default::default()
            }
        };
        session.tick(&course, input);
    }
    assert_ne!(session.phase, RunPhase::Finished);
    let mut coordinated = false;
    complete_demo(&course, |playback| {
        let input = playback.displayed_input();
        coordinated |= input.strafe > 0.0 && input.yaw < -0.35 && !playback.session.player.grounded;
    });
    assert!(coordinated);
}

#[test]
fn ramp_transfer_records_both_faces() {
    let course = tutorial::course("tutorial-transfer").unwrap();
    let playback = complete_demo(&course, |_| {});
    assert_eq!(playback.session.splits.len(), 2);
}

#[test]
fn kicker_launches_the_player_to_the_raised_deck() {
    let course = tutorial::course("tutorial-rise").unwrap();
    let mut rose_off_the_lip = false;
    complete_demo(&course, |playback| {
        let p = playback.session.player.position;
        let v = playback.session.player.velocity;
        rose_off_the_lip |= p.z < -1400.0 && v.y > 65.0;
    });
    assert!(rose_off_the_lip);
}

#[test]
fn linking_a_line_requires_a_left_face_transfer_and_high_exit() {
    let course = tutorial::course("tutorial-line").unwrap();
    let mut rose_off_the_lip = false;
    let playback = complete_demo(&course, |playback| {
        let session = &playback.session;
        rose_off_the_lip |= session.player.position.z < -2860.0 && session.player.velocity.y > 65.0;
    });
    assert_eq!(playback.session.splits.len(), 2);
    assert!(rose_off_the_lip);
}

#[test]
fn held_jump_chains_both_pads_from_the_launch_stripe() {
    let course = tutorial::course("tutorial-bhop").unwrap();
    let mut session = Session::new(&course);
    let mut jumped = false;
    let mut reached_first_pad = false;
    for _ in 0..300 {
        let jump = !jumped && session.player.position.z <= -220.0;
        jumped |= jump;
        session.tick(
            &course,
            MoveInput {
                forward: 1.0,
                jump,
                ..Default::default()
            },
        );
        reached_first_pad |= session.splits.len() == 1;
        assert_ne!(session.phase, RunPhase::Finished);
    }
    assert!(
        reached_first_pad,
        "a single jump should reach the first pad"
    );

    for launch in [-196.0, -220.0, -246.0] {
        session.restart(&course);
        let mut landed_on = [false; 2];
        complete(&course, &mut session, |session| {
            let z = session.player.position.z;
            if session.player.grounded {
                landed_on[0] |= (-462.0..=-370.0).contains(&z);
                landed_on[1] |= (-660.0..=-550.0).contains(&z);
            }
            MoveInput {
                forward: 1.0,
                jump: z <= launch && z > -748.0,
                ..Default::default()
            }
        });
        assert_eq!(landed_on, [true; 2], "launch z={launch}");
        assert_eq!(session.splits.len(), 2);
        assert!(!session.ranked_eligible());
    }
    complete_demo(&course, |_| {});
}

fn complete_demo(course: &Course, mut observe: impl FnMut(&Playback)) -> Playback {
    let replay = tutorial::demonstration(course)
        .unwrap_or_else(|| panic!("{} does not produce a demo", course.name));
    let mut playback = Playback::new(course, replay).unwrap();
    while playback.tick(course) {
        observe(&playback);
    }
    let session = &playback.session;
    let lesson = tutorial::lesson(&course.name).unwrap();
    assert_eq!(session.phase, RunPhase::Finished);
    assert!(session.player.grounded);
    assert_eq!(session.splits.len() + 1, lesson.sections.len());
    assert_eq!(
        lesson.medal(session.elapsed),
        Some(tutorial::Medal::Gold),
        "{} demo in {:.3}s",
        lesson.name,
        session.elapsed
    );
    assert!(!session.ranked_eligible());
    playback
}

fn complete(course: &Course, session: &mut Session, mut inputs: impl FnMut(&Session) -> MoveInput) {
    let mut last_z = session.player.position.z;
    for _ in 0..660 {
        let input = inputs(session);
        session.tick(course, input);
        if session.phase == RunPhase::Finished {
            assert!(
                session.player.grounded,
                "{} finished before landing",
                course.name
            );
            let lesson = tutorial::lesson(&course.name).unwrap();
            assert_eq!(session.splits.len() + 1, lesson.sections.len());
            assert_eq!(
                lesson.medal(session.elapsed),
                Some(tutorial::Medal::Gold),
                "{} reached in {:.3}s, gold target {:.3}s",
                course.name,
                session.elapsed,
                lesson.gold_seconds
            );
            return;
        }
        if session.player.position == course.spawn && last_z < -400.0 {
            panic!(
                "{} fell below the course after reaching z={last_z}",
                course.name
            );
        }
        last_z = session.player.position.z;
    }
    panic!("{} did not finish: {:?}", course.name, session.player);
}
