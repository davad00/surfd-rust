use std::{
    f32::consts::{PI, TAU},
    fmt,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    course::Course,
    movement::{MoveInput, TICK_SECONDS},
    reference_ghost::ReferenceGhostPose,
    rules::PhysicsProfile,
    session::{RunOptions, RunPhase, Session},
};

/// A versioned command stream for the native port. Ranking always uses the
/// server's simulation and canonical course, never a client-supplied time.
pub const REPLAY_VERSION: u16 = 2;
pub const MAX_COMMANDS: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TickCommand {
    pub input: MoveInput,
    pub pitch: f32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restart_stage: bool,
    /// Original Source view yaw before conversion to the renderer's f32 radians.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_yaw_degrees: Option<f64>,
}

impl TickCommand {
    pub(crate) fn valid(self) -> bool {
        let input = self.input;
        input.forward.abs() <= 1.0
            && input.strafe.abs() <= 1.0
            && input.yaw.abs() <= std::f32::consts::TAU
            && self.pitch.abs() <= std::f32::consts::FRAC_PI_2
            && self.source_yaw_degrees.is_none_or(|degrees| {
                let yaw = (degrees - 90.0).to_radians();
                let turn = std::f64::consts::TAU;
                let difference = (yaw - f64::from(input.yaw)).rem_euclid(turn);
                degrees.abs() <= 360.0
                    && difference.min(turn - difference) <= 8.0 * f64::from(f32::EPSILON)
            })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunReplay {
    pub version: u16,
    pub map: String,
    pub revision: String,
    pub bonus: Option<u16>,
    pub options: RunOptions,
    pub commands: Vec<TickCommand>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerifiedRun {
    pub time_us: u64,
    pub splits_us: Vec<u64>,
    pub checkpoint_speeds: Vec<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<VerifiedStage>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerifiedStage {
    pub number: u16,
    pub time_us: u64,
    pub start_tick: f64,
    pub end_tick: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayError {
    Version,
    Course,
    Unranked,
    Length,
    Command(usize),
    TrailingCommands,
    Incomplete,
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version => f.write_str("Unsupported replay version"),
            Self::Course => f.write_str("Replay course or board revision does not match"),
            Self::Unranked => f.write_str("This course or ruleset is not ranked"),
            Self::Length => f.write_str("Replay command count is outside the supported range"),
            Self::Command(index) => write!(f, "Invalid replay command at tick {index}"),
            Self::TrailingCommands => f.write_str("Replay contains commands after the finish"),
            Self::Incomplete => f.write_str("Replay did not complete the route"),
        }
    }
}

impl std::error::Error for ReplayError {}

fn validate_header(course: &Course, replay: &RunReplay) -> Result<PhysicsProfile, ReplayError> {
    if !matches!(replay.version, 1 | REPLAY_VERSION) {
        return Err(ReplayError::Version);
    }
    if course.name != replay.map {
        return Err(ReplayError::Course);
    }
    if replay.commands.is_empty() || replay.commands.len() > MAX_COMMANDS {
        return Err(ReplayError::Length);
    }
    course
        .replay_physics_profile(replay.bonus, &replay.revision)
        .ok_or(ReplayError::Course)
}

fn validate(course: &Course, replay: &RunReplay) -> Result<PhysicsProfile, ReplayError> {
    let profile = validate_header(course, replay)?;
    if let Some(index) = replay.commands.iter().position(|command| {
        !command.valid() || (replay.version == 1 && command.source_yaw_degrees.is_some())
    }) {
        return Err(ReplayError::Command(index));
    }
    Ok(profile)
}

pub struct Playback {
    pub session: Session,
    pub yaw: f32,
    pub pitch: f32,
    pub cursor: usize,
    replay: RunReplay,
    span: Option<(f64, f64)>,
}

impl Playback {
    pub fn new(course: &Course, replay: RunReplay) -> Result<Self, ReplayError> {
        let profile = validate(course, &replay)?;
        Self::from_validated(course, replay, profile)
    }

    fn from_validated(
        course: &Course,
        replay: RunReplay,
        profile: PhysicsProfile,
    ) -> Result<Self, ReplayError> {
        let session = Session::for_playback(course, replay.options, replay.bonus, profile)
            .map_err(|_| ReplayError::Course)?;
        Ok(Self {
            yaw: session.spawn_yaw(course),
            pitch: session.spawn_pitch(course),
            cursor: 0,
            session,
            replay,
            span: None,
        })
    }

    pub fn len(&self) -> usize {
        self.span
            .map_or(self.replay.commands.len(), |(_, end)| end.ceil() as usize)
    }

    pub fn is_empty(&self) -> bool {
        self.replay.commands.is_empty()
    }

    pub fn restart(&mut self, course: &Course) {
        self.session.restart(course);
        self.cursor = 0;
        self.yaw = self.session.spawn_yaw(course);
        self.pitch = self.session.spawn_pitch(course);
        if let Some((start, _)) = self.span {
            while self.cursor < start.floor() as usize && self.tick(course) {}
        }
    }

    pub fn set_span(
        &mut self,
        course: &Course,
        start_tick: f64,
        end_tick: f64,
    ) -> Result<(), ReplayError> {
        if !start_tick.is_finite()
            || !end_tick.is_finite()
            || start_tick < 0.0
            || end_tick <= start_tick
            || end_tick > self.replay.commands.len() as f64
        {
            return Err(ReplayError::Length);
        }
        self.span = Some((start_tick, end_tick));
        self.restart(course);
        Ok(())
    }

    pub fn progress(&self) -> (usize, usize) {
        let start = self.span.map_or(0, |(start, _)| start.floor() as usize);
        (self.cursor.saturating_sub(start), self.len() - start)
    }

    pub fn span_elapsed(&self) -> Option<f64> {
        let (start, end) = self.span?;
        Some((self.cursor as f64 - start).clamp(0.0, end - start) / crate::movement::TICK_HZ)
    }

    pub fn displayed_input(&self) -> MoveInput {
        self.cursor
            .checked_sub(1)
            .map_or_else(MoveInput::default, |index| {
                self.replay.commands[index].input
            })
    }

    /// Advance one recorded command. Returns false once all commands were played.
    pub fn tick(&mut self, course: &Course) -> bool {
        if self.cursor >= self.len() {
            return false;
        }
        let Some(command) = self.replay.commands.get(self.cursor) else {
            return false;
        };
        self.yaw = self
            .session
            .tick_command(course, *command)
            .unwrap_or(command.input.yaw);
        self.pitch = self.session.pitch_override().unwrap_or(command.pitch);
        self.cursor += 1;
        true
    }
}

struct GhostFrame {
    pose: ReferenceGhostPose,
    snap: bool,
}

/// Samples a native recording on the live run clock without simulating it every tick.
pub struct NativeGhost {
    frames: Vec<GhostFrame>,
    start_tick: f64,
    end_tick: f64,
}

/// Builds the same pose track over bounded portions of several application frames.
pub struct NativeGhostPreparation {
    playback: Option<Playback>,
    frames: Vec<GhostFrame>,
    validated: usize,
    command_count: usize,
}

impl NativeGhostPreparation {
    pub fn new(course: &Course, replay: RunReplay) -> Result<Self, ReplayError> {
        let profile = validate_header(course, &replay)?;
        let playback = Playback::from_validated(course, replay, profile)?;
        let command_count = playback.len();
        let mut frames = Vec::with_capacity(command_count + 1);
        frames.push(GhostFrame::capture(&playback));
        Ok(Self {
            playback: Some(playback),
            frames,
            validated: 0,
            command_count,
        })
    }

    pub fn progress(&self) -> (usize, usize) {
        let cursor = self
            .playback
            .as_ref()
            .map_or(self.command_count, |playback| playback.cursor);
        (self.validated + cursor, self.command_count * 2)
    }

    pub fn advance(
        &mut self,
        course: &Course,
        budget: Duration,
        max_commands: usize,
    ) -> Result<Option<NativeGhost>, ReplayError> {
        let started = Instant::now();
        for _ in 0..max_commands {
            if started.elapsed() >= budget {
                break;
            }
            let Some(playback) = &mut self.playback else {
                break;
            };
            if self.validated < self.command_count {
                let command = playback.replay.commands[self.validated];
                if !command.valid()
                    || (playback.replay.version == 1 && command.source_yaw_degrees.is_some())
                {
                    return Err(ReplayError::Command(self.validated));
                }
                self.validated += 1;
                continue;
            }
            if !playback.tick(course) {
                return Err(ReplayError::Incomplete);
            }
            self.frames.push(GhostFrame::capture(playback));
            if playback.session.phase == RunPhase::Finished {
                let start_tick = playback
                    .session
                    .replay_start_tick(playback.cursor)
                    .ok_or(ReplayError::Incomplete)?;
                let end_tick = (start_tick + playback.session.elapsed / f64::from(TICK_SECONDS))
                    .min(playback.cursor as f64);
                self.playback = None;
                return Ok(Some(NativeGhost {
                    frames: std::mem::take(&mut self.frames),
                    start_tick,
                    end_tick,
                }));
            }
        }
        Ok(None)
    }
}

impl GhostFrame {
    fn capture(playback: &Playback) -> Self {
        Self {
            pose: ReferenceGhostPose {
                position: playback.session.player.position,
                yaw: playback.yaw,
                crouched: playback.session.player.is_crouched(),
            },
            snap: playback.session.pose_discontinuity(),
        }
    }
}

impl NativeGhost {
    pub fn new(course: &Course, replay: RunReplay) -> Result<Self, ReplayError> {
        let mut preparation = NativeGhostPreparation::new(course, replay)?;
        loop {
            if let Some(ghost) = preparation.advance(course, Duration::MAX, usize::MAX)? {
                return Ok(ghost);
            }
        }
    }

    pub fn sample(&self, elapsed: f64) -> Option<ReferenceGhostPose> {
        if !elapsed.is_finite() {
            return None;
        }
        let tick =
            (self.start_tick + elapsed.max(0.0) / f64::from(TICK_SECONDS)).min(self.end_tick);
        let nearest = tick.round();
        // The seconds-to-ticks conversion can round an exact frame across a teleport boundary.
        let tick = if (tick - nearest).abs() <= 1e-9 {
            nearest
        } else {
            tick
        };
        let index = tick.floor() as usize;
        let alpha = (tick - index as f64) as f32;
        let before = self.frames[index].pose;
        if alpha == 0.0 {
            return Some(before);
        }
        let next = &self.frames[index + 1];
        let after = next.pose;
        if next.snap {
            return Some(after);
        }
        let turn = (after.yaw - before.yaw + PI).rem_euclid(TAU) - PI;
        Some(ReferenceGhostPose {
            position: before.position.lerp(after.position, alpha),
            yaw: (before.yaw + turn * alpha).rem_euclid(TAU),
            crouched: if alpha < 0.5 {
                before.crouched
            } else {
                after.crouched
            },
        })
    }
}

pub fn verify_ranked(course: &Course, replay: &RunReplay) -> Result<VerifiedRun, ReplayError> {
    if replay.version != REPLAY_VERSION {
        return Err(ReplayError::Version);
    }
    if course.catalog_revision.is_none() || replay.options != RunOptions::default() {
        return Err(ReplayError::Unranked);
    }
    let profile = course.physics_profile_for(replay.bonus);
    if validate(course, replay)? != profile {
        return Err(ReplayError::Course);
    }
    let mut session = Session::for_playback(course, replay.options, replay.bonus, profile)
        .map_err(|_| ReplayError::Course)?;
    if !session.ranked_eligible() {
        return Err(ReplayError::Unranked);
    }
    for (index, command) in replay.commands.iter().enumerate() {
        session.tick_command(course, *command);
        if session.phase == RunPhase::Finished && index + 1 != replay.commands.len() {
            return Err(ReplayError::TrailingCommands);
        }
    }
    if session.phase != RunPhase::Finished || !session.ranked_eligible() {
        return Err(ReplayError::Incomplete);
    }
    let micros = |seconds: f64| (seconds * 1_000_000.0).round() as u64;
    let time_us = micros(session.elapsed);
    if time_us == 0 {
        return Err(ReplayError::Incomplete);
    }
    let stages = session
        .verified_stages()
        .iter()
        .map(|stage| VerifiedStage {
            number: stage.stage,
            time_us: stage.time_us,
            start_tick: stage.start_tick,
            end_tick: stage.end_tick,
        })
        .collect();
    Ok(VerifiedRun {
        time_us,
        splits_us: session.splits.into_iter().map(micros).collect(),
        checkpoint_speeds: session.checkpoint_speeds,
        stages,
    })
}

#[cfg(test)]
mod tests {
    use bevy_math::{DVec3, Vec3};

    use super::*;
    use crate::course::{BonusRoute, Brush, PracticeSpawn, Zone, ZoneKind};

    fn zone(z: f32, kind: ZoneKind) -> Zone {
        Zone {
            brush: Brush::box_at(
                Vec3::new(-80.0, -8.0, z - 8.0),
                Vec3::new(80.0, 120.0, z + 8.0),
            ),
            kind,
        }
    }

    fn course() -> Course {
        let mut course = Course::practice();
        course.spawn = Vec3::Y * 0.05;
        course.spawn_yaw = 0.0;
        course.brushes = vec![Brush::box_at(
            Vec3::new(-400.0, -50.0, -500.0),
            Vec3::new(400.0, 0.0, 500.0),
        )];
        course.zones = vec![
            zone(0.0, ZoneKind::Start),
            zone(-75.0, ZoneKind::Checkpoint(1)),
            zone(-140.0, ZoneKind::Finish),
        ];
        course.bonus_routes.push(BonusRoute {
            number: 1,
            name: "Bonus 1".to_owned(),
            spawn: PracticeSpawn {
                name: "Bonus 1".to_owned(),
                position: course.spawn,
                yaw: 0.0,
                pitch: 0.0,
                source_yaw_degrees: None,
            },
            zones: vec![zone(0.0, ZoneKind::Start), zone(-240.0, ZoneKind::Finish)],
            revision: "test-bonus-v1".to_owned(),
        });
        course
    }

    fn finish(course: &Course, bonus: Option<u16>) -> (RunReplay, f64) {
        finish_with_yaw(course, bonus, None)
    }

    fn finish_with_yaw(
        course: &Course,
        bonus: Option<u16>,
        source_yaw_degrees: Option<f64>,
    ) -> (RunReplay, f64) {
        let mut session = Session::new(course);
        session.select_bonus(course, bonus).unwrap();
        let yaw = source_yaw_degrees.map_or(0.0, crate::movement::source_yaw_to_native);
        for _ in 0..200 {
            session.tick_command(
                course,
                TickCommand {
                    input: MoveInput {
                        forward: 1.0,
                        yaw,
                        ..Default::default()
                    },
                    pitch: 0.12,
                    restart_stage: false,
                    source_yaw_degrees,
                },
            );
            if session.phase == RunPhase::Finished {
                return (
                    session.take_finished_replay(course).unwrap(),
                    session.elapsed,
                );
            }
        }
        panic!("flat fixture failed to complete");
    }

    #[test]
    fn verifier_resimulates_a_json_replay_and_rejects_altered_inputs_or_revision() {
        let mut course = course();
        let (replay, elapsed) = finish(&course, None);
        let json = serde_json::to_vec(&replay).unwrap();
        assert!(
            !std::str::from_utf8(&json)
                .unwrap()
                .contains("source_yaw_degrees")
        );
        let decoded = serde_json::from_slice::<RunReplay>(&json).unwrap();
        let verified = verify_ranked(&course, &decoded).unwrap();
        assert_eq!(verified.time_us, (elapsed * 1e6).round() as u64);
        assert_eq!(verified.splits_us.len(), 1);
        assert!(verified.checkpoint_speeds[0] > 100.0);
        let mut playback = Playback::new(&course, decoded.clone()).unwrap();
        while playback.tick(&course) {}
        assert_eq!(playback.session.phase, RunPhase::Finished);
        assert_eq!(playback.session.elapsed, elapsed);

        let mut legacy = decoded.clone();
        legacy.version = 1;
        assert!(Playback::new(&course, legacy.clone()).is_ok());
        assert_eq!(verify_ranked(&course, &legacy), Err(ReplayError::Version));

        let mut bad = decoded.clone();
        bad.commands[0].input.strafe = 100.0;
        assert_eq!(verify_ranked(&course, &bad), Err(ReplayError::Command(0)));
        let mut bad = decoded.clone();
        bad.commands.push(decoded.commands[0]);
        assert_eq!(
            verify_ranked(&course, &bad),
            Err(ReplayError::TrailingCommands)
        );
        course.catalog_revision = None;
        assert_eq!(verify_ranked(&course, &decoded), Err(ReplayError::Unranked));
    }

    #[test]
    fn authored_contact_board_accepts_new_runs_and_plays_legacy_runs_with_the_old_timer() {
        let mut course = course();
        let (legacy, legacy_elapsed) = finish(&course, None);
        course.map_triggers.vphysics_feature_fix = true;
        let start = &mut course.zones[0].brush;
        start.planes =
            start
                .planes
                .clone()
                .with_zone_geometry(crate::course::AuthoredZoneGeometry {
                    min: DVec3::new(-80.0, -8.0, -8.000_000_25),
                    max: start.max.as_dvec3(),
                    planes: Vec::new(),
                });
        let (current, current_elapsed) = finish(&course, None);
        assert_ne!(legacy_elapsed, current_elapsed);
        assert_ne!(legacy.revision, current.revision);
        assert_eq!(verify_ranked(&course, &legacy), Err(ReplayError::Course));
        assert!(verify_ranked(&course, &current).is_ok());

        let mut playback = Playback::new(&course, legacy).unwrap();
        while playback.tick(&course) {}
        assert_eq!(playback.session.elapsed, legacy_elapsed);
        playback.restart(&course);
        while playback.tick(&course) {}
        assert_eq!(playback.session.elapsed, legacy_elapsed);

        course.map_triggers.surfd_rules = true;
        let (updated, _) = finish(&course, None);
        assert_ne!(updated.revision, current.revision);
        assert_eq!(verify_ranked(&course, &current), Err(ReplayError::Course));
        assert!(verify_ranked(&course, &updated).is_ok());
        let mut playback = Playback::new(&course, current).unwrap();
        while playback.tick(&course) {}
        assert_eq!(playback.session.elapsed, current_elapsed);
    }

    #[test]
    fn precise_live_yaw_survives_serialization_and_ranked_verification() {
        let course = course();
        let source_yaw_degrees = 89.9876543210457;
        let (replay, elapsed) = finish_with_yaw(&course, None, Some(source_yaw_degrees));
        let mut decoded: RunReplay =
            serde_json::from_slice(&serde_json::to_vec(&replay).unwrap()).unwrap();
        assert_eq!(
            decoded.commands[0].source_yaw_degrees,
            Some(source_yaw_degrees)
        );
        assert_eq!(
            verify_ranked(&course, &decoded).unwrap().time_us,
            (elapsed * 1e6).round() as u64
        );

        decoded.commands[0].source_yaw_degrees = Some(source_yaw_degrees + 0.01);
        assert_eq!(
            verify_ranked(&course, &decoded),
            Err(ReplayError::Command(0))
        );
    }

    #[test]
    fn timed_bonus_verifies_only_against_its_own_board() {
        let course = course();
        let (main, _) = finish(&course, None);
        let (mut bonus, _) = finish(&course, Some(1));
        let main = verify_ranked(&course, &main).unwrap();
        let verified_bonus = verify_ranked(&course, &bonus).unwrap();
        assert!(verified_bonus.time_us > main.time_us);
        assert!(verified_bonus.splits_us.is_empty());
        bonus.bonus = None;
        assert_eq!(verify_ranked(&course, &bonus), Err(ReplayError::Course));
    }

    #[test]
    fn stage_playback_limits_the_stream_and_keeps_the_fractional_clock() {
        let course = course();
        let (replay, _) = finish(&course, None);
        let mut playback = Playback::new(&course, replay).unwrap();
        playback.set_span(&course, 2.25, 5.75).unwrap();
        assert_eq!(playback.cursor, 2);
        assert_eq!(playback.progress(), (0, 4));
        assert_eq!(playback.span_elapsed(), Some(0.0));
        while playback.tick(&course) {}
        assert_eq!(playback.progress(), (4, 4));
        assert_eq!(
            playback.span_elapsed(),
            Some(3.5 / crate::movement::TICK_HZ)
        );
        playback.restart(&course);
        assert_eq!(playback.cursor, 2);
    }

    #[test]
    fn native_ghost_aligns_at_equal_run_times_after_waiting_and_reset() {
        let mut course = course();
        course.zones.push(zone(
            -96.0,
            ZoneKind::Teleport {
                position: Vec3::new(0.0, 0.05, -124.0),
                yaw: None,
            },
        ));
        course.zones.push(Zone {
            brush: Brush::box_at(Vec3::new(48.0, -8.0, -100.0), Vec3::new(56.0, 120.0, -30.0)),
            kind: ZoneKind::Reset,
        });
        let (mut replay, finish_time) = finish(&course, None);
        let idle = TickCommand {
            input: MoveInput::default(),
            pitch: 0.0,
            restart_stage: false,
            source_yaw_degrees: None,
        };
        replay.commands.splice(..0, std::iter::repeat_n(idle, 90));
        let ghost = NativeGhost::new(&course, replay).unwrap();
        let start = ghost.sample(0.0).unwrap();
        assert!((start.position.z + 24.0).abs() < 1e-4);
        assert_eq!(ghost.sample(finish_time), ghost.sample(finish_time + 1.0));

        let (teleport, destination) = ghost
            .frames
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, frame)| frame.snap)
            .expect("the replay should cross the teleport");
        let mid_teleport = (teleport as f64 - 0.5 - ghost.start_tick) * f64::from(TICK_SECONDS);
        assert_eq!(ghost.sample(mid_teleport), Some(destination.pose));

        let mut immediate = Session::new(&course);
        let mut delayed = Session::new(&course);
        for _ in 0..120 {
            delayed.tick(&course, MoveInput::default());
            assert_eq!(delayed.phase, RunPhase::Ready);
            assert_eq!(ghost.sample(delayed.elapsed), Some(start));
        }
        let forward = MoveInput {
            forward: 1.0,
            ..Default::default()
        };
        for _ in 0..30 {
            immediate.tick(&course, forward);
            delayed.tick(&course, forward);
            assert_eq!(
                ghost.sample(immediate.elapsed),
                ghost.sample(delayed.elapsed)
            );
            if delayed.phase == RunPhase::Running && delayed.elapsed > 0.08 {
                break;
            }
        }
        assert_eq!(delayed.phase, RunPhase::Running);
        assert!(
            ghost
                .sample(delayed.elapsed)
                .unwrap()
                .position
                .distance(delayed.player.position)
                < 1e-4
        );

        delayed.player.velocity.x = 3000.0;
        delayed.tick(&course, forward);
        assert_eq!(delayed.phase, RunPhase::Ready);
        assert_eq!(ghost.sample(delayed.elapsed), Some(start));
    }

    #[test]
    fn incremental_ghost_preparation_limits_work_and_matches_the_run_clock() {
        let course = course();
        let (replay, finish_time) = finish(&course, None);
        let mut preparation = NativeGhostPreparation::new(&course, replay.clone()).unwrap();
        let mut ghost = None;
        for _ in 0..replay.commands.len() * 2 {
            let before = preparation.progress().0;
            ghost = preparation.advance(&course, Duration::MAX, 7).unwrap();
            assert!(preparation.progress().0 - before <= 7);
            if ghost.is_some() {
                break;
            }
        }
        let ghost = ghost.expect("bounded preparation should finish");
        assert!((ghost.sample(0.0).unwrap().position.z + 24.0).abs() < 1e-4);
        assert!((ghost.sample(finish_time).unwrap().position.z + 116.0).abs() < 1e-4);

        let mut invalid = replay;
        let bad_index = invalid.commands.len() - 1;
        invalid.commands[bad_index].input.forward = 2.0;
        let mut preparation = NativeGhostPreparation::new(&course, invalid).unwrap();
        assert!(
            preparation
                .advance(&course, Duration::MAX, bad_index)
                .unwrap()
                .is_none()
        );
        assert!(
            matches!(preparation.advance(&course, Duration::MAX, 1), Err(ReplayError::Command(index)) if index == bad_index)
        );
    }
}
