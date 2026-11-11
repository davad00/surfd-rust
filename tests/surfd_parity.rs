use std::{env, fs::File, io::BufReader};

use bevy_math::Vec3;
use bevy_surf::{
    movement::{MoveInput, TICK_HZ},
    replay::TickCommand,
    rules::MovementRules,
    session::{RecordedInitialState, RunOptions, RunPhase, Session},
    surfd_map,
};
use serde::{Deserialize, de::DeserializeOwned};

#[derive(Deserialize)]
struct Point {
    x: f32,
    y: f32,
    z: f32,
}

impl Point {
    fn world(&self) -> Vec3 {
        Vec3::new(self.x, self.z, -self.y)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OriginalReplay {
    map_id: String,
    map_version: String,
    format: u8,
    initial_state: InitialState,
    config: Config,
    commands: Vec<Command>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitialState {
    position: Point,
    velocity: Point,
    grounded: bool,
    ground_id: Option<String>,
    ducked: bool,
    duck_fraction: f32,
    jump_held: bool,
    surface_friction: f32,
    yaw: f64,
    pitch: f64,
    tick: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    dt: f64,
    max_velocity: f32,
    auto_bhop: bool,
    #[serde(default)]
    unrestricted_start: bool,
    #[serde(default)]
    incline_fix: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Command {
    forward: f64,
    side: f64,
    yaw: f64,
    pitch: f64,
    jump: bool,
    duck: bool,
    #[serde(default)]
    restart_stage: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoldenGhost {
    map_id: String,
    map_version: String,
    dt: f64,
    start_tick: f64,
    end_tick: f64,
    completed: Option<bool>,
    positions: Vec<f32>,
    view: Option<GoldenView>,
}

#[derive(Deserialize)]
struct GoldenView {
    splits: Vec<f64>,
}

fn fixture<T: DeserializeOwned>(name: &str) -> T {
    let path = env::var(name).unwrap_or_else(|_| panic!("Set {name} to its fixture file"));
    let file = File::open(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    serde_json::from_reader(BufReader::new(file)).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn yaw(value: f64) -> f32 {
    (value - 90.0).to_radians() as f32
}

fn pitch(value: f64) -> f32 {
    -value.to_radians() as f32
}

/// Run with `cargo test --no-default-features --test surfd_parity -- --ignored --nocapture`.
/// Supply the original collision export, input replay JSON, and original-worker
/// ghost JSON for the same completed route using the `SURFD_PARITY_*` variables.
#[test]
#[ignore = "requires SURFD_PARITY_GEOMETRY, SURFD_PARITY_REPLAY, and SURFD_PARITY_GHOST fixtures"]
fn original_surfd_commands_match_native_trajectory_and_timing() {
    let geometry = env::var_os("SURFD_PARITY_GEOMETRY").expect("Set SURFD_PARITY_GEOMETRY");
    let mut course = surfd_map::load_collision(geometry.as_ref()).expect("Load original collision");
    let replay: OriginalReplay = fixture("SURFD_PARITY_REPLAY");
    let golden: GoldenGhost = fixture("SURFD_PARITY_GHOST");
    assert_eq!(replay.map_id, course.name);
    assert_eq!(golden.map_id, course.name);
    assert_eq!(replay.map_version, golden.map_version);
    assert_eq!(replay.config.dt, golden.dt);
    assert_eq!(replay.config.dt as f32, TICK_HZ.recip() as f32);
    let rules = MovementRules::for_course(&course);
    assert_eq!(replay.config.max_velocity, rules.max_velocity);
    assert_eq!(replay.config.incline_fix, rules.incline_fix);
    assert_eq!(golden.positions.len() % 3, 0);
    let frames = golden.positions.len() / 3;
    assert!((2..=replay.commands.len() + 1).contains(&frames));

    let mut session = Session::with_options(
        &course,
        RunOptions {
            auto_bhop: replay.config.auto_bhop,
            unrestricted_start: replay.config.unrestricted_start,
        },
    );
    if let Some((_, suffix)) = replay.map_version.rsplit_once("/bonus-") {
        let number = suffix
            .split('.')
            .next()
            .unwrap()
            .parse::<u16>()
            .expect("Bonus number");
        let pack = env::var_os("SURFD_PARITY_BONUS_PACK").expect("Set SURFD_PARITY_BONUS_PACK");
        surfd_map::add_bonus_spawns(&mut course, pack.as_ref())
            .expect("Load original bonus routes");
        session
            .select_bonus(&course, Some(number))
            .expect("Select original bonus route");
    }
    let start = replay.initial_state;
    let state = RecordedInitialState {
        position: start.position.world(),
        velocity: start.velocity.world(),
        grounded: start.grounded,
        ground_id: start.ground_id,
        crouched: start.ducked,
        crouch_fraction: start.duck_fraction,
        jump_held: start.jump_held,
        surface_friction: start.surface_friction,
        yaw: yaw(start.yaw),
        pitch: pitch(start.pitch),
        tick: start.tick,
    };
    if env::var_os("SURFD_PARITY_LIVE_RULES").is_some() {
        session.set_replay_initial_state(&course, state)
    } else {
        session.set_reference_replay_initial_state(&course, state, replay.format)
    }
    .expect("Restore recorded initial player state");

    let mut first_difference = None;
    let mut max_error = 0.0_f32;
    let mut native_finish_tick = None;
    for (tick, expected) in golden.positions.chunks_exact(3).enumerate() {
        if let Some(index) = tick.checked_sub(1) {
            let command = &replay.commands[index];
            session.tick_reference_command(
                &course,
                TickCommand {
                    input: MoveInput {
                        forward: (command.forward / 400.0) as f32,
                        strafe: (command.side / 400.0) as f32,
                        yaw: yaw(command.yaw),
                        jump: command.jump,
                        crouch: command.duck,
                    },
                    pitch: pitch(command.pitch),
                    restart_stage: replay.format >= 7 && command.restart_stage,
                    source_yaw_degrees: Some(command.yaw),
                },
                [command.forward, command.side],
            );
        }
        let expected = Vec3::new(expected[0], expected[2], -expected[1]);
        let error = session.player.position.distance(expected);
        if error > 0.0 {
            first_difference.get_or_insert((tick, error));
            max_error = max_error.max(error);
        }
        if session.phase == RunPhase::Finished {
            native_finish_tick.get_or_insert(tick);
        }
    }
    let golden_duration = (golden.completed != Some(false))
        .then(|| (golden.end_tick - golden.start_tick) * golden.dt);
    println!(
        "{} compared {frames} frames. First position difference {first_difference:?}, largest error {max_error:.6} units.",
        golden.map_id
    );
    println!(
        "Native finish frame {native_finish_tick:?}, time {:.12}, splits {:?}. Reference time {golden_duration:?}, splits {:?}.",
        session.elapsed,
        session.splits,
        golden.view.as_ref().map(|view| &view.splits)
    );
    assert_eq!(first_difference, None, "The movement paths differ");
    assert_eq!(native_finish_tick, golden_duration.map(|_| frames - 1));
    if let Some(duration) = golden_duration {
        assert!((session.elapsed - duration).abs() <= 1e-9);
    }
    if let Some(view) = golden.view {
        assert_eq!(session.splits.len(), view.splits.len());
        for (native, reference) in session.splits.iter().zip(view.splits) {
            assert!((native - reference).abs() <= 1e-9);
        }
    }
}
