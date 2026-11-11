use std::f32::consts::FRAC_PI_2;

use crate::{
    course::Course,
    movement::{MoveInput, native_yaw_to_source},
    replay::{RunReplay, TickCommand},
    session::{RunPhase, Session},
};

use super::lesson;

/// Record the authored walkthrough through the same fixed-step simulation as live play.
pub fn demonstration(course: &Course) -> Option<RunReplay> {
    let lesson = lesson(&course.name)?;
    let mut session = Session::new(course);
    let mut air_ticks = 0;
    let mut corner_yaw = 0.0;
    for _ in 0..660 {
        let input = match lesson.id {
            "tutorial-movement" => walk_and_jump(&session, &mut corner_yaw),
            "tutorial-catch" => right_face(&session, false),
            "tutorial-strafe" => air_strafe(&session, &mut air_ticks),
            "tutorial-transfer" => transfer(&session, false),
            "tutorial-rise" => right_face(&session, true),
            "tutorial-line" => transfer(&session, true),
            "tutorial-bhop" => bunnyhop(&session),
            _ => return None,
        };
        session.tick_command(
            course,
            TickCommand {
                input,
                pitch: 0.0,
                restart_stage: false,
                source_yaw_degrees: Some(native_yaw_to_source(input.yaw)),
            },
        );
        if session.phase == RunPhase::Finished {
            return session
                .player
                .grounded
                .then(|| session.take_finished_replay(course))
                .flatten();
        }
    }
    None
}

fn walk_and_jump(session: &Session, yaw: &mut f32) -> MoveInput {
    let p = session.player.position;
    if p.z < -341.0 || *yaw < 0.0 {
        *yaw = (*yaw - FRAC_PI_2 / 8.0).max(-FRAC_PI_2);
    }
    MoveInput {
        forward: 1.0,
        yaw: *yaw,
        jump: (320.0..410.0).contains(&p.x),
        ..Default::default()
    }
}

fn right_face(session: &Session, kicker: bool) -> MoveInput {
    let p = session.player.position;
    let v = session.player.velocity;
    let (lip, landing, outside, drift) = if kicker {
        (-1400.0, -1500.0, 76.0, 22.0)
    } else {
        (-1410.0, -1470.0, 120.0, 28.0)
    };
    let mut input = MoveInput::default();
    if p.z > -250.0 || (p.z < landing && session.player.grounded) {
        input.forward = 1.0;
    } else if p.z > lip && (p.x > outside || v.x > drift) {
        input.strafe = -1.0;
    }
    input
}

fn air_strafe(session: &Session, ticks: &mut u16) -> MoveInput {
    if session.player.position.y > 517.0 {
        return MoveInput {
            forward: 1.0,
            ..Default::default()
        };
    }
    // Keep the new heading as the player lands. The turn and the strafe end separately.
    let yaw = -(f32::from(*ticks) * 0.03).min(0.70);
    if session.player.grounded {
        return MoveInput {
            forward: 1.0,
            yaw,
            ..Default::default()
        };
    }
    *ticks += 1;
    MoveInput {
        strafe: if *ticks <= 30 { 1.0 } else { 0.0 },
        yaw: -(f32::from(*ticks) * 0.03).min(0.70),
        ..Default::default()
    }
}

fn transfer(session: &Session, mirrored: bool) -> MoveInput {
    let side = if mirrored { -1.0 } else { 1.0 };
    let (exit, touch, lip, landing) = if mirrored {
        (-1460.0, -1785.0, -2860.0, -2960.0)
    } else {
        (-1240.0, -1510.0, -2480.0, -2530.0)
    };
    let p = session.player.position;
    let v = session.player.velocity;
    let mut input = MoveInput::default();
    if p.z > -250.0 || (p.z < landing && session.player.grounded) {
        input.forward = 1.0;
    } else if p.z > exit + 75.0 {
        if side * p.x > 116.0 || side * v.x > 26.0 {
            input.strafe = -side;
        }
    } else if p.z > touch + 15.0 {
        input.strafe = side;
        input.yaw = -side * 0.07;
    } else if p.z > lip && (side * p.x < 235.0 || side * v.x < -25.0) {
        input.strafe = side;
    }
    input
}

fn bunnyhop(session: &Session) -> MoveInput {
    let z = session.player.position.z;
    MoveInput {
        forward: 1.0,
        jump: z <= -220.0 && z > -748.0,
        ..Default::default()
    }
}
