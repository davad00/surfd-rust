use std::{collections::HashSet, sync::Arc};

use bevy_math::{DVec3, Vec3};
use serde::{Deserialize, Serialize};

use crate::{
    brush_index::BrushIndex,
    course::{
        AuthoredTeleport, BonusCollision, BonusRules, Brush, Course, PlayerOutput,
        PlayerOutputValue, PracticeSpawn, Zone, ZoneKind,
    },
    movement::{
        GroundCheck, HALF_GRAVITY_STEP, MoveInput, MovementSegment, Player, RUN_SPEED, StepRules,
        TICK_HZ, step_with_index,
    },
    replay::{MAX_COMMANDS, REPLAY_VERSION, RunReplay, TickCommand},
    rules::{MovementRules, PhysicsProfile},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPhase {
    Untimed,
    Ready,
    Running,
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOptions {
    pub auto_bhop: bool,
    pub unrestricted_start: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            auto_bhop: true,
            unrestricted_start: false,
        }
    }
}

/// The player state saved before an original Surfd command recording starts.
/// Coordinates use the native basis and view angles are in radians.
#[derive(Clone, Debug)]
pub struct RecordedInitialState {
    pub position: Vec3,
    pub velocity: Vec3,
    pub grounded: bool,
    pub ground_id: Option<String>,
    pub crouched: bool,
    pub crouch_fraction: f32,
    pub jump_held: bool,
    pub surface_friction: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub tick: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageResult {
    pub stage: u16,
    pub time_us: u64,
    pub start_tick: f64,
    pub end_tick: f64,
}

#[derive(Default)]
struct StageClock {
    stage_index: usize,
    latest_start: Option<f64>,
    earliest_start: f64,
    clamp_after: f64,
    awaiting_handoff: bool,
    results: Vec<StageResult>,
}

#[derive(Clone)]
struct SavedPose {
    player: Player,
    yaw: f32,
    pitch: f32,
    source_yaw_degrees: Option<f64>,
    bonus: Option<u16>,
    options: RunOptions,
    triggers: TriggerState,
}

#[derive(Clone, Copy)]
struct StageHandoff {
    position: Vec3,
    yaw: f32,
    pitch: f32,
    source_yaw_degrees: Option<f64>,
    stage: u16,
}

#[derive(Clone, Default)]
struct TriggerState {
    tick: u64,
    stage: u16,
    pending_stage: Option<StageHandoff>,
    last_map_teleport: Option<u64>,
    base_velocity: Option<Vec3>,
    pending_push: Option<Vec3>,
    carried_push: Option<Vec3>,
    ground_check: Option<GroundCheck>,
    targetname: String,
    output_contacts: HashSet<usize>,
    accepted_outputs: HashSet<usize>,
    accepted_pushes: HashSet<usize>,
    gravity_scale: Option<f32>,
    movement_scale: Option<f32>,
    jump_disabled: bool,
    pending_outputs: Vec<(u64, PlayerOutputValue)>,
    stage_start_recovery: bool,
}

impl TriggerState {
    fn at_start() -> Self {
        Self {
            stage: 1,
            ..Self::default()
        }
    }
}

pub struct Session {
    pub player: Player,
    pub phase: RunPhase,
    /// Current or finished run time in seconds.
    pub elapsed: f64,
    pub best: Option<f64>,
    /// Elapsed seconds at each completed checkpoint, in course order.
    pub splits: Vec<f64>,
    pub checkpoint_speeds: Vec<f32>,
    run_start_tick: Option<f64>,
    stage_clock: StageClock,
    stage_checkpoint_handoffs: Vec<bool>,
    checkpoint_order: Vec<u16>,
    initial_phase: RunPhase,
    last_teleport: Option<usize>,
    practice: Option<PracticeSpawn>,
    saved: Option<SavedPose>,
    bonus: Option<u16>,
    options: RunOptions,
    rules: MovementRules,
    catalog_board: bool,
    ranked_valid: bool,
    recorded_playback: bool,
    playback_profile: Option<PhysicsProfile>,
    reference_replay_format: Option<u8>,
    commands: Option<Vec<TickCommand>>,
    recording_overflow: bool,
    start_jumps: u32,
    start_ground_time: f32,
    start_region: Option<u16>,
    previous_yaw: f32,
    sync_matching: u32,
    sync_measured: u32,
    triggers: TriggerState,
    pitch_override: Option<f32>,
    source_yaw_override: Option<f64>,
    pose_discontinuity: bool,
    pose_revision: u32,
    brush_index: Option<Arc<BrushIndex>>,
    bonus_collision: Option<Arc<BonusCollision>>,
    brush_candidates: Vec<usize>,
}

impl Session {
    pub fn new(course: &Course) -> Self {
        Self::with_options(course, RunOptions::default())
    }

    /// Pull the camera toward its eye anchor before it intersects world geometry.
    pub fn clip_camera(&mut self, course: &Course, eye: Vec3, target: Vec3) -> Vec3 {
        let (brushes, index) = self.bonus_collision.as_deref().map_or(
            (course.brushes.as_slice(), self.brush_index.as_deref()),
            |collision| (collision.brushes.as_slice(), collision.index.as_deref()),
        );
        crate::movement::clip_camera(eye, target, brushes, index, &mut self.brush_candidates)
    }

    pub fn with_options(course: &Course, options: RunOptions) -> Self {
        let mut session = Self {
            player: Player::new(course.spawn),
            phase: RunPhase::Untimed,
            elapsed: 0.0,
            best: None,
            splits: Vec::new(),
            checkpoint_speeds: Vec::new(),
            run_start_tick: None,
            stage_clock: StageClock::default(),
            stage_checkpoint_handoffs: Vec::new(),
            checkpoint_order: Vec::new(),
            initial_phase: RunPhase::Untimed,
            last_teleport: None,
            practice: None,
            saved: None,
            bonus: None,
            options,
            rules: MovementRules::for_course(course),
            catalog_board: false,
            ranked_valid: true,
            recorded_playback: false,
            playback_profile: None,
            reference_replay_format: None,
            commands: Some(Vec::new()),
            recording_overflow: false,
            start_jumps: 0,
            start_ground_time: 0.0,
            start_region: None,
            previous_yaw: course.spawn_yaw,
            sync_matching: 0,
            sync_measured: 0,
            triggers: TriggerState::at_start(),
            pitch_override: None,
            source_yaw_override: None,
            pose_discontinuity: false,
            pose_revision: 0,
            brush_index: course.collision_index(),
            bonus_collision: None,
            brush_candidates: Vec::new(),
        };
        session.refresh_board(course);
        session.restart(course);
        session
    }

    pub(crate) fn for_playback(
        course: &Course,
        options: RunOptions,
        bonus: Option<u16>,
        profile: PhysicsProfile,
    ) -> Result<Self, &'static str> {
        let mut session = Self::with_options(course, options);
        session.commands = None;
        session.playback_profile = Some(profile);
        session.select_bonus(course, bonus)?;
        Ok(session)
    }

    fn physics_profile(&self, course: &Course) -> PhysicsProfile {
        self.playback_profile
            .unwrap_or_else(|| course.physics_profile_for(self.bonus))
    }

    fn uses_precise_triggers(&self, course: &Course) -> bool {
        self.reference_replay_format.is_some()
            || self.physics_profile(course) != PhysicsProfile::Legacy
    }

    /// Restore the original pre-command state for playback without producing a ranked recording.
    pub fn set_replay_initial_state(
        &mut self,
        course: &Course,
        state: RecordedInitialState,
    ) -> Result<(), &'static str> {
        if !state.position.is_finite()
            || !state.velocity.is_finite()
            || !state.yaw.is_finite()
            || !state.pitch.is_finite()
            || !(0.0..=1.0).contains(&state.crouch_fraction)
            || !(0.0..=1.0).contains(&state.surface_friction)
            || state.tick > u64::MAX - MAX_COMMANDS as u64 - u64::from(u32::MAX)
        {
            return Err("Replay contains an invalid initial player state");
        }
        let ground_brush =
            state.ground_id.as_deref().and_then(|id| {
                let (brushes, recorded) = self.bonus_collision.as_deref().map_or(
                    (&course.brushes, &course.map_triggers.recorded_brushes),
                    |collision| (&collision.brushes, &collision.recorded_brushes),
                );
                recorded
                    .iter()
                    .position(|brush| brush.id.as_deref() == Some(id))
                    .or_else(|| {
                        let (surface, triangle) = id.strip_prefix("disp-")?.split_once('-')?;
                        let ids = (surface.parse().ok()?, triangle.parse().ok()?);
                        brushes.iter().enumerate().skip(recorded.len()).find_map(
                            |(index, brush)| {
                                (brush.planes.displacement_id() == Some(ids)).then_some(index)
                            },
                        )
                    })
            });
        self.restart(course);
        self.player.apply_recorded_state(&state, ground_brush);
        self.triggers.tick = state.tick;
        self.previous_yaw = state.yaw;
        self.pitch_override = Some(state.pitch);
        self.source_yaw_override = Some(crate::movement::native_yaw_to_source(state.yaw));
        self.commands = None;
        self.recorded_playback = true;
        self.reference_replay_format = None;
        Ok(())
    }

    /// Restore a Surfd recording using the rules indicated by its format.
    pub fn set_reference_replay_initial_state(
        &mut self,
        course: &Course,
        state: RecordedInitialState,
        format: u8,
    ) -> Result<(), &'static str> {
        self.set_replay_initial_state(course, state)?;
        self.reference_replay_format = Some(format);
        Ok(())
    }

    pub fn checkpoint_count(&self) -> usize {
        self.checkpoint_order.len()
    }

    /// Independent stage times for a completed staged route.
    pub fn verified_stages(&self) -> &[StageResult] {
        if self.phase == RunPhase::Finished
            && self.stage_clock.results.len() == self.stage_checkpoint_handoffs.len()
        {
            &self.stage_clock.results
        } else {
            &[]
        }
    }

    pub fn options(&self) -> RunOptions {
        self.options
    }

    pub fn selected_bonus(&self) -> Option<u16> {
        self.bonus
    }

    pub fn set_options(&mut self, course: &Course, options: RunOptions) {
        if self.options != options {
            self.options = options;
            self.best = None;
            self.restart(course);
        }
    }

    pub fn select_bonus(
        &mut self,
        course: &Course,
        bonus: Option<u16>,
    ) -> Result<(), &'static str> {
        if bonus.is_some_and(|number| course.bonus(number).is_none()) {
            return Err("This bonus is unavailable");
        }
        if self.bonus != bonus {
            self.bonus = bonus;
            self.bonus_collision = if self.physics_profile(course) == PhysicsProfile::SurfdBonus {
                course.bonus_collision(bonus)
            } else {
                None
            };
            self.best = None;
            self.refresh_board(course);
        }
        self.practice = None;
        self.restart(course);
        Ok(())
    }

    pub fn ranked_eligible(&self) -> bool {
        self.catalog_board
            && self.ranked_valid
            && !self.recorded_playback
            && self.options == RunOptions::default()
            && self.practice.is_none()
            && self.initial_phase == RunPhase::Ready
    }

    pub fn mark_unranked(&mut self) {
        if self.phase != RunPhase::Finished {
            self.ranked_valid = false;
        }
    }

    pub fn strafe_sync(&self) -> Option<f32> {
        (self.sync_measured > 0).then(|| self.sync_matching as f32 / self.sync_measured as f32)
    }

    /// Select an untimed practice start, or return to the selected route's start.
    pub fn set_practice(&mut self, course: &Course, spawn: Option<&PracticeSpawn>) {
        self.practice = spawn.cloned();
        self.restart(course);
    }

    /// Saving a pose does not change this run's eligibility.
    pub fn save_position(&mut self, yaw: f32, pitch: f32) -> bool {
        self.save_position_with_source_yaw(yaw, pitch, None)
    }

    /// Save the rendered view and its full precision Source yaw together.
    pub fn save_position_with_source_yaw(
        &mut self,
        yaw: f32,
        pitch: f32,
        source_yaw_degrees: Option<f64>,
    ) -> bool {
        if self.phase == RunPhase::Finished || !yaw.is_finite() || !pitch.is_finite() {
            return false;
        }
        self.saved = Some(SavedPose {
            player: self.player,
            yaw,
            pitch,
            source_yaw_degrees: source_yaw_degrees.filter(|yaw| yaw.abs() <= 360.0),
            bonus: self.bonus,
            options: self.options,
            triggers: self.triggers.clone(),
        });
        true
    }

    pub fn has_saved_position(&self) -> bool {
        self.saved
            .as_ref()
            .is_some_and(|pose| pose.bonus == self.bonus && pose.options == self.options)
    }

    /// Resume a saved movement state in untimed practice, including its velocity and crouch.
    pub fn restore_position(&mut self, course: &Course) -> Option<(f32, f32)> {
        let pose = self
            .saved
            .as_ref()
            .filter(|pose| pose.bonus == self.bonus && pose.options == self.options)?
            .clone();
        self.practice = Some(PracticeSpawn {
            name: String::new(),
            position: pose.player.position,
            yaw: pose.yaw,
            pitch: pose.pitch,
            source_yaw_degrees: pose.source_yaw_degrees,
        });
        self.restart(course);
        self.player = pose.player;
        self.triggers = pose.triggers;
        Some((pose.yaw, pose.pitch))
    }

    pub fn spawn_yaw(&self, course: &Course) -> f32 {
        self.practice.as_ref().map_or_else(
            || {
                self.bonus
                    .and_then(|n| course.bonus(n))
                    .map_or(course.spawn_yaw, |route| route.spawn.yaw)
            },
            |spawn| spawn.yaw,
        )
    }

    pub fn spawn_pitch(&self, course: &Course) -> f32 {
        self.practice.as_ref().map_or_else(
            || {
                self.bonus
                    .and_then(|n| course.bonus(n))
                    .map_or(course.map_triggers.spawn_pitch.unwrap_or(-0.19), |route| {
                        route.spawn.pitch
                    })
            },
            |spawn| spawn.pitch,
        )
    }

    pub fn spawn_source_yaw(&self, course: &Course) -> f64 {
        let precise = if let Some(spawn) = &self.practice {
            spawn.source_yaw_degrees
        } else if let Some(route) = self.bonus.and_then(|number| course.bonus(number)) {
            route.spawn.source_yaw_degrees
        } else {
            course.map_triggers.spawn_source_yaw
        };
        precise.unwrap_or_else(|| crate::movement::native_yaw_to_source(self.spawn_yaw(course)))
    }

    pub fn pitch_override(&self) -> Option<f32> {
        self.pitch_override
    }

    pub fn source_yaw_override(&self) -> Option<f64> {
        self.source_yaw_override
    }

    pub(crate) fn replay_start_tick(&self, cursor: usize) -> Option<f64> {
        // A map reset restarts the simulation tick counter, but playback keeps its cursor.
        self.run_start_tick
            .map(|start| cursor as f64 - self.triggers.tick as f64 + start)
    }

    pub(crate) fn pose_discontinuity(&self) -> bool {
        self.pose_discontinuity
    }

    /// Remains changed after a snap even when several simulation ticks pass between room poses.
    pub fn pose_revision(&self) -> u32 {
        self.pose_revision
    }

    fn mark_pose_discontinuity(&mut self) {
        self.pose_discontinuity = true;
        self.pose_revision = self.pose_revision.wrapping_add(1);
    }

    /// Restart this route and keep its best time for the current movement rules.
    pub fn restart(&mut self, course: &Course) {
        let route_spawn = self
            .bonus
            .and_then(|n| course.bonus(n))
            .map_or(course.spawn, |route| route.spawn.position);
        let position = self
            .practice
            .as_ref()
            .map_or(route_spawn, |spawn| spawn.position);
        self.player = Player::new(position);
        self.mark_pose_discontinuity();
        self.clear_run();
        self.last_teleport = None;
        self.start_jumps = 0;
        self.start_ground_time = 0.0;
        self.start_region = None;
        self.previous_yaw = self.spawn_yaw(course);
        self.ranked_valid = true;
        self.recording_overflow = false;
        self.triggers = TriggerState::at_start();
        self.pitch_override = Some(self.spawn_pitch(course));
        self.source_yaw_override = Some(self.spawn_source_yaw(course));
        if let Some(commands) = &mut self.commands {
            commands.clear();
        }
    }

    /// Consume one simulation tick to return to the current authored stage start.
    /// The running clock and completed checkpoint splits remain active.
    pub fn restart_stage(&mut self, course: &Course) -> Result<(f32, f32), &'static str> {
        if self.phase != RunPhase::Running {
            return Err("Restart stage is available during a run");
        }
        if self.triggers.pending_stage.is_some() {
            return Err("Finishing the stage transition");
        }
        let stage = self.triggers.stage;
        if self.bonus.is_some() || usize::from(stage) != self.splits.len() + 1 {
            return Err("Stage progress is unavailable");
        }
        let spawn = course
            .map_triggers
            .authored_stage_spawns
            .get(usize::from(stage - 1))
            .ok_or("This course has no current stage start")?;
        self.triggers = TriggerState {
            tick: self.triggers.tick + 1,
            stage,
            stage_start_recovery: stage == 1,
            ..TriggerState::default()
        };
        self.player = Player::at_stage_restart(spawn.position);
        self.mark_pose_discontinuity();
        self.start_jumps = 0;
        self.start_ground_time = 0.0;
        self.start_region = None;
        self.previous_yaw = spawn.yaw;
        self.pitch_override = Some(spawn.pitch);
        self.source_yaw_override = spawn.source_yaw_degrees;
        self.elapsed = self.time_at(1.0, 0.0);
        self.observe_stage_clock(course, &[], 1.0);
        Ok((spawn.yaw, spawn.pitch))
    }

    /// Advance one movement tick and return a view yaw override if the map supplies one.
    pub fn tick(&mut self, course: &Course, input: MoveInput) -> Option<f32> {
        self.tick_with_view(course, input, 0.0)
    }

    pub fn tick_with_view(&mut self, course: &Course, input: MoveInput, pitch: f32) -> Option<f32> {
        self.tick_command(
            course,
            TickCommand {
                input,
                pitch,
                restart_stage: false,
                source_yaw_degrees: None,
            },
        )
    }

    /// Advance imported replay playback using the original command angle in degrees.
    pub fn tick_recorded_command(
        &mut self,
        course: &Course,
        mut command: TickCommand,
        yaw_degrees: f64,
    ) -> Option<f32> {
        self.commands = None;
        self.recorded_playback = true;
        command.source_yaw_degrees = Some(yaw_degrees);
        self.tick_command(course, command)
    }

    pub fn tick_command(&mut self, course: &Course, command: TickCommand) -> Option<f32> {
        self.advance_tick(course, command, None)
    }

    /// Replay the original command axes without rounding them through native input storage.
    pub fn tick_reference_command(
        &mut self,
        course: &Course,
        command: TickCommand,
        axes: [f64; 2],
    ) -> Option<f32> {
        self.commands = None;
        self.recorded_playback = true;
        self.advance_tick(course, command, Some(axes))
    }

    fn advance_tick(
        &mut self,
        course: &Course,
        command: TickCommand,
        recorded_axes: Option<[f64; 2]>,
    ) -> Option<f32> {
        self.capture_command(command);
        self.source_yaw_override = None;
        self.pose_discontinuity = false;
        if command.restart_stage
            && let Ok((yaw, _)) = self.restart_stage(course)
        {
            return Some(yaw);
        }
        let input = command.input;
        self.pitch_override = None;
        if let Some(handoff) = self.triggers.pending_stage.take() {
            self.triggers.stage = handoff.stage;
            self.triggers.tick += 1;
            self.triggers.pending_push = None;
            self.triggers.base_velocity = None;
            self.start_jumps = 0;
            self.start_ground_time = 0.0;
            self.start_region = None;
            self.player.position = handoff.position;
            self.mark_pose_discontinuity();
            self.player.velocity = Vec3::NEG_Y * HALF_GRAVITY_STEP;
            self.player.detach_ground();
            if course.map_triggers.stage_handoff_releases_duck && !input.crouch {
                self.player.release_crouch();
            }
            self.pitch_override = Some(handoff.pitch);
            self.source_yaw_override = handoff.source_yaw_degrees;
            self.previous_yaw = handoff.yaw;
            if self.phase == RunPhase::Running {
                self.elapsed = self.time_at(1.0, 0.0);
            }
            self.observe_stage_clock(course, &[], 1.0);
            return Some(handoff.yaw);
        }
        let from = self.player.position;
        let precise_triggers = self.uses_precise_triggers(course);
        let hull = TriggerHull::for_player(&self.player, precise_triggers);
        let map = &course.map_triggers;
        let profile = self.physics_profile(course);
        let bonus = (profile == PhysicsProfile::SurfdBonus)
            .then(|| course.bonus_rules(self.bonus))
            .flatten();
        let movement_volumes = &map.movement_volumes;
        let bonus_volumes = bonus.map(|rules| &rules.movement_volumes);
        let ladder_ids = bonus_volumes
            .map(|volumes| &volumes.ladder_ids)
            .filter(|ids| !ids.is_empty())
            .unwrap_or(&movement_volumes.ladder_ids);
        let water_hulls = bonus_volumes
            .map(|volumes| volumes.water_hulls.as_slice())
            .filter(|hulls| !hulls.is_empty())
            .unwrap_or(&movement_volumes.water_hulls);
        let format = self.reference_replay_format;
        let zones = self.zones(course);
        let effects = bonus
            .and_then(|rules| rules.effects.as_deref())
            .unwrap_or(&course.zones);
        let stage = self.triggers.stage;
        let previous_stage_start = start_brush(zones, &ZoneKind::StageStart(stage), from, hull);
        let start_kind = if stage == 1 && previous_stage_start.is_some() {
            ZoneKind::StageStart(1)
        } else {
            ZoneKind::Start
        };
        let previous_start = start_brush(zones, &start_kind, from, hull);
        let restricted = !self.options.unrestricted_start;
        let limit_prehops = restricted && format.is_none_or(|format| format >= 2);
        if limit_prehops && previous_start.is_some() {
            let region = Some(u16::from(matches!(start_kind, ZoneKind::StageStart(_))));
            if self.start_region != region {
                self.start_jumps = 0;
                self.start_ground_time = 0.0;
                self.start_region = region;
            }
        }
        let blocked_start_jump = if limit_prehops && input.jump && self.start_jumps >= 1 {
            previous_start
        } else {
            None
        };
        let no_jump_scale = effects.iter().find_map(|zone| {
            if let ZoneKind::NoJump(scale) = zone.kind
                && hull.touches(zone, from)
            {
                Some(scale)
            } else {
                None
            }
        });
        let push = self.map_push(course, effects, bonus, no_jump_scale);
        let (brushes, recorded, friction, index) = self.bonus_collision.as_deref().map_or(
            (
                &course.brushes,
                &map.recorded_brushes,
                &map.surface_friction,
                self.brush_index.as_deref(),
            ),
            |collision| {
                (
                    &collision.brushes,
                    &collision.recorded_brushes,
                    &collision.friction,
                    collision.index.as_deref(),
                )
            },
        );
        let movement = step_with_index(
            &mut self.player,
            input,
            brushes,
            index,
            &mut self.brush_candidates,
            StepRules {
                movement: self.rules,
                auto_bhop: self.options.auto_bhop,
                blocked_start_jump,
                no_jump_scale,
                movement_scale: self.triggers.movement_scale,
                gravity_scale: self.triggers.gravity_scale,
                jump_disabled: self.triggers.jump_disabled,
                push,
                source_push_ground_fix: course.map_triggers.source_push_ground_fix,
                recheck_ground: self.triggers.ground_check.take(),
                friction,
                recorded_brushes: recorded,
                vphysics_feature_fix: map.vphysics_feature_fix
                    && format.map_or(profile != PhysicsProfile::Legacy, |format| {
                        format >= map.vphysics_contact_format
                    }),
                bsp_corner_fix: map.bsp_corner_fix
                    && format.map_or(profile.uses_surfd(), |format| format >= 7),
                exact_bonus_contacts: bonus.is_some(),
                downhill_landing: format.map_or(profile.uses_surfd(), |format| format >= 8),
                recorded_yaw: command.source_yaw_degrees.filter(|yaw| yaw.abs() <= 360.0),
                recorded_axes,
                pitch: command.pitch,
                ladder_ids: (profile.uses_surfd() && !ladder_ids.is_empty()).then_some(ladder_ids),
                water_hulls: if profile.uses_surfd() {
                    water_hulls
                } else {
                    &[]
                },
            },
        );
        self.triggers.tick += 1;
        if movement.cleared_gravity_scale {
            self.triggers.gravity_scale = None;
        }
        if !self.player.position.is_finite() || movement.blocked {
            self.restart(course);
            return Some(self.spawn_yaw(course));
        }
        self.update_accepted_pushes(effects, bonus, precise_triggers);
        if course.map_triggers.push_after_movement {
            let pending = push_in_zones(
                effects,
                &self.player,
                precise_triggers,
                bonus,
                &self.triggers.accepted_pushes,
            );
            if course.map_triggers.source_push_ground_fix
                && pending.is_some_and(|push| push.y > 0.0)
                && self.player.grounded
            {
                self.player.position.y += 1.0;
                self.player.detach_ground();
                self.triggers.ground_check = Some(GroundCheck::Push);
            }
            self.triggers.pending_push = pending;
        }
        self.update_player_outputs(effects, precise_triggers);

        if movement.jumped {
            self.start_jumps = self.start_jumps.saturating_add(1);
        }
        let to = self.player.position;
        let hull = TriggerHull::for_player(&self.player, precise_triggers);
        let speed = self.player.velocity.x.hypot(self.player.velocity.z);
        let inside_start = start_brush(zones, &ZoneKind::Start, to, hull).is_some();
        let settled = restricted
            && previous_start.is_some()
            && start_brush(zones, &start_kind, to, hull).is_some()
            && self.player.grounded
            && !input.jump
            && speed <= RUN_SPEED + 0.01;
        self.start_ground_time = if settled {
            self.start_ground_time + 0.015
        } else {
            0.0
        };
        if self.start_ground_time >= 0.1 {
            self.start_jumps = 0;
        }

        let events = zone_events(
            zones,
            movement.segments.as_slice(),
            from,
            to,
            hull,
            &self.triggers.targetname,
            bonus.is_some(),
        );
        let mut running_from = 0.0;
        let mut view_yaw = None;
        let mut started = false;
        let mut finished = false;
        let mut teleported = false;
        let mut segment_end = 1.0;
        for (at, event) in events {
            match event {
                ZoneEvent::StartEntry
                    if self.phase == RunPhase::Running && !self.triggers.stage_start_recovery =>
                {
                    self.clear_run();
                    self.triggers.stage = 1;
                }
                ZoneEvent::StartExit if self.phase == RunPhase::Ready => {
                    self.phase = RunPhase::Running;
                    let start_tick = self.triggers.tick.saturating_sub(1) as f64 + at;
                    self.run_start_tick = Some(start_tick);
                    self.stage_clock.latest_start = Some(start_tick);
                    self.stage_clock.earliest_start = start_tick;
                    self.stage_clock.clamp_after = start_tick;
                    running_from = at;
                    started = true;
                }
                ZoneEvent::Checkpoint(order)
                    if self.phase == RunPhase::Running
                        && self.checkpoint_order.get(self.splits.len()) == Some(&order) =>
                {
                    self.splits.push(self.time_at(at, running_from));
                    let velocity = movement
                        .velocity_before_downhill_landing
                        .unwrap_or(self.player.velocity);
                    self.checkpoint_speeds.push(velocity.x.hypot(velocity.z));
                    if self.bonus.is_none() && course.map_triggers.continuous_stages {
                        self.triggers.stage = (self.splits.len() + 1) as u16;
                    }
                }
                ZoneEvent::Finish
                    if self.phase == RunPhase::Running
                        && self.splits.len() == self.checkpoint_count() =>
                {
                    self.elapsed = self.time_at(at, running_from);
                    self.best = Some(
                        self.best
                            .map_or(self.elapsed, |best| best.min(self.elapsed)),
                    );
                    self.phase = RunPhase::Finished;
                    segment_end = at;
                    finished = true;
                }
                ZoneEvent::Reset if self.phase != RunPhase::Finished => {
                    self.restart(course);
                    return Some(self.spawn_yaw(course));
                }
                ZoneEvent::Teleport {
                    index,
                    position,
                    yaw,
                    clear_velocity,
                } if !finished && (at > 0.0 || self.last_teleport != Some(index)) => {
                    self.player.position = position;
                    self.mark_pose_discontinuity();
                    self.player.detach_ground();
                    if clear_velocity {
                        self.player.velocity = Vec3::ZERO;
                        self.start_jumps = 0;
                        self.start_ground_time = 0.0;
                        self.start_region = None;
                    }
                    self.last_teleport = Some(index);
                    view_yaw = yaw;
                    teleported = true;
                    segment_end = at;
                    break;
                }
                ZoneEvent::AuthoredTeleport(teleport) if !finished => {
                    if let Some(velocity) = movement.velocity_before_downhill_landing {
                        self.player.velocity = velocity;
                    }
                    if teleport.restart {
                        self.restart(course);
                        return Some(self.spawn_yaw(course));
                    }
                    let out_of_order = teleport.checkpoint
                        && (teleport.stage_velocity || teleport.preserve_velocity)
                        && teleport
                            .stage
                            .is_some_and(|next| next != self.triggers.stage + 1);
                    if out_of_order {
                        self.mark_unranked();
                    }
                    if teleport.stage_velocity
                        && let Some(stage) = teleport.stage
                    {
                        self.triggers.pending_stage = Some(StageHandoff {
                            position: teleport.position,
                            yaw: teleport.yaw,
                            pitch: teleport.pitch,
                            source_yaw_degrees: teleport.source_yaw_degrees,
                            stage,
                        });
                        self.triggers.pending_push = None;
                    } else {
                        if (course.map_triggers.telehop_fix
                            || bonus.is_some_and(|rules| rules.telehop_fix))
                            && teleport.preserve_velocity
                            && self.triggers.last_map_teleport != self.triggers.tick.checked_sub(1)
                            && (movement.had_contact || movement.landed)
                        {
                            let limit = Vec3::splat(self.rules.max_velocity);
                            let horizontal_push = push.unwrap_or_default().with_y(0.0);
                            self.player.velocity = (movement.pre_collision_velocity
                                - horizontal_push
                                - Vec3::Y * HALF_GRAVITY_STEP)
                                .clamp(-limit, limit);
                        }
                        self.triggers.last_map_teleport = Some(self.triggers.tick);
                        if bonus.is_some() && teleport.carry_push {
                            self.triggers.carried_push = push_in_zones(
                                effects,
                                &self.player,
                                precise_triggers,
                                bonus,
                                &self.triggers.accepted_pushes,
                            );
                        }
                        self.player.position = match teleport
                            .landmark
                            .filter(|_| teleport.stage.is_none())
                        {
                            Some(landmark) => (self.player.position - landmark) + teleport.position,
                            None => teleport.position,
                        };
                        self.mark_pose_discontinuity();
                        self.player.detach_ground();
                        if teleport.relative {
                            self.player.velocity = Vec3::ZERO;
                        }
                        if teleport.stage_velocity {
                            self.player.velocity = Vec3::NEG_Y * HALF_GRAVITY_STEP;
                        }
                        if let Some(stage) = teleport.stage {
                            self.triggers.stage = stage;
                        }
                        if course.map_triggers.source_push_ground_fix {
                            self.triggers.ground_check = Some(GroundCheck::Teleport);
                        } else {
                            self.triggers.pending_push = None;
                            self.triggers.base_velocity = None;
                        }
                        if !teleport.keep_view
                            && !teleport.relative
                            && (teleport.landmark.is_none() || teleport.stage.is_some())
                        {
                            view_yaw = Some(teleport.yaw);
                            self.pitch_override = Some(teleport.pitch);
                            self.source_yaw_override = teleport.source_yaw_degrees;
                        }
                    }
                    teleported = true;
                    segment_end = at;
                    break;
                }
                _ => {}
            }
        }
        let left_stage = previous_stage_start.is_some()
            && start_brush(
                zones,
                &ZoneKind::StageStart(stage),
                self.player.position,
                hull,
            )
            .is_none();
        let boosted = bonus.is_some_and(|rules| rules.boosted_start)
            || (self.bonus.is_none()
                && course
                    .map_triggers
                    .boosted_start_stages
                    .contains(&self.triggers.stage));
        if restricted && !boosted && (started || left_stage) {
            self.cap_start_speed(profile.uses_surfd() || format.is_some());
        }

        let position = self.player.position;
        let kill_y = bonus
            .and_then(|rules| rules.fall_y)
            .unwrap_or(course.kill_y);
        if position.y < kill_y || position.x.abs().max(position.z.abs()) > 32768.0 {
            self.restart(course);
            return Some(self.spawn_yaw(course));
        }
        if self.phase == RunPhase::Running {
            self.elapsed = self.time_at(1.0, running_from);
        }
        if self.triggers.stage_start_recovery && !inside_start {
            self.triggers.stage_start_recovery = false;
        }
        self.observe_stage_clock(course, movement.segments.as_slice(), segment_end);
        if self.phase == RunPhase::Running || finished {
            self.measure_strafe(input, inside_start, started, teleported);
        }
        self.previous_yaw = view_yaw.unwrap_or(input.yaw);
        view_yaw
    }

    pub fn take_finished_replay(&mut self, course: &Course) -> Option<RunReplay> {
        if self.phase != RunPhase::Finished || self.recording_overflow {
            return None;
        }
        let commands = self
            .commands
            .as_mut()
            .filter(|commands| !commands.is_empty())?;
        Some(RunReplay {
            version: REPLAY_VERSION,
            map: course.name.clone(),
            revision: course.replay_revision(self.bonus)?,
            bonus: self.bonus,
            options: self.options,
            commands: std::mem::take(commands),
        })
    }

    fn map_push(
        &mut self,
        course: &Course,
        effects: &[Zone],
        bonus: Option<&BonusRules>,
        no_jump_scale: Option<f32>,
    ) -> Option<Vec3> {
        let map = &course.map_triggers;
        let mut push = if let Some(carried) = self.triggers.carried_push.take() {
            Some(carried)
        } else if map.push_after_movement {
            self.triggers.pending_push
        } else {
            push_in_zones(
                effects,
                &self.player,
                self.uses_precise_triggers(course),
                bonus,
                &self.triggers.accepted_pushes,
            )
        };
        if self.player.grounded
            && let Some(ground) = self.player.ground_brush
        {
            let conveyors = self
                .bonus_collision
                .as_deref()
                .map_or(&map.conveyors, |collision| &collision.conveyors);
            for conveyor in conveyors {
                if conveyor.brush_indices.contains(&ground) {
                    *push.get_or_insert(Vec3::ZERO) += conveyor.velocity;
                }
            }
        }
        if let Some(push) = push {
            self.triggers.base_velocity = Some(push.with_y(0.0));
        } else if let Some(base) = self.triggers.base_velocity.take() {
            let dt = TICK_HZ.recip() as f32 * no_jump_scale.unwrap_or(1.0);
            let mut impulse = base * (1.0 + dt * 0.5);
            if bonus.is_some_and(|rules| rules.impulse_limit)
                && impulse.abs().max_element() >= 4000.0
            {
                let length = ((impulse.x * impulse.x + impulse.z * impulse.z)
                    + impulse.y * impulse.y)
                    .sqrt();
                impulse *= 4000.0 / length;
            }
            self.player.velocity += impulse;
            if map.output_velocity_ground_fix && self.player.velocity.y > 140.0 {
                self.player.detach_ground();
            }
        }
        push
    }

    fn update_accepted_pushes(
        &mut self,
        zones: &[Zone],
        bonus: Option<&BonusRules>,
        precise: bool,
    ) {
        let Some(rules) = bonus.filter(|rules| !rules.push_filters.is_empty()) else {
            return;
        };
        let hull = TriggerHull::for_player(&self.player, precise);
        let position = self.player.position;
        self.triggers.accepted_pushes = zones
            .iter()
            .filter_map(|zone| {
                let ZoneKind::Push { index, .. } = zone.kind else {
                    return None;
                };
                let filter = rules.push_filters.get(&index)?;
                (hull.touches(zone, position)
                    && (self.triggers.accepted_pushes.contains(&index)
                        || filter.accepts(&self.triggers.targetname)))
                .then_some(index)
            })
            .collect();
    }

    fn update_player_outputs(&mut self, zones: &[Zone], precise: bool) {
        let ready = self
            .triggers
            .pending_outputs
            .partition_point(|(tick, _)| *tick <= self.triggers.tick);
        let due = self
            .triggers
            .pending_outputs
            .drain(..ready)
            .collect::<Vec<_>>();
        for (_, value) in due {
            self.apply_player_output(value);
        }
        let position = self.player.position;
        let hull = TriggerHull::for_player(&self.player, precise);
        let contacts: HashSet<_> = zones
            .iter()
            .filter_map(|zone| {
                if let ZoneKind::PlayerOutput(output) = &zone.kind
                    && hull.touches(zone, position)
                {
                    Some(output.index)
                } else {
                    None
                }
            })
            .collect();
        let mut seen = HashSet::new();
        let mut actions = Vec::<&PlayerOutput>::new();
        for zone in zones {
            let ZoneKind::PlayerOutput(output) = &zone.kind else {
                continue;
            };
            if !seen.insert(output.index) {
                continue;
            }
            if contacts.contains(&output.index) {
                if !self.triggers.output_contacts.contains(&output.index)
                    && output.filter_multi.as_ref().map_or_else(
                        || {
                            output
                                .filter
                                .as_ref()
                                .is_none_or(|filter| filter.accepts(&self.triggers.targetname))
                        },
                        |filter| filter.accepts(&self.triggers.targetname),
                    )
                {
                    self.triggers.accepted_outputs.insert(output.index);
                    actions.extend(&output.on_start);
                }
                actions.extend(&output.on_touch);
            } else if self.triggers.accepted_outputs.remove(&output.index) {
                actions.extend(&output.on_end);
            }
        }
        self.triggers.output_contacts = contacts;
        let mut scheduled = false;
        for action in actions {
            if action.delay_ticks == 0 {
                self.apply_player_output(action.value.clone());
            } else {
                let tick = self.triggers.tick + u64::from(action.delay_ticks);
                self.triggers
                    .pending_outputs
                    .push((tick, action.value.clone()));
                scheduled = true;
            }
        }
        if scheduled {
            self.triggers.pending_outputs.sort_by_key(|(tick, _)| *tick);
        }
    }

    fn apply_player_output(&mut self, value: PlayerOutputValue) {
        match value {
            PlayerOutputValue::TargetName(value) => self.triggers.targetname = value,
            PlayerOutputValue::Velocity(value) => self.player.velocity = value,
            PlayerOutputValue::BaseVelocity(value) => self.triggers.base_velocity = Some(value),
            PlayerOutputValue::Gravity(value) => {
                self.triggers.gravity_scale = (value != 0.0 && value != 1.0).then_some(value);
            }
            PlayerOutputValue::LaggedMovement {
                scale,
                suppress_jump,
            } => {
                self.triggers.movement_scale = (scale != 1.0).then_some(scale);
                if suppress_jump {
                    self.triggers.jump_disabled = scale != 1.0;
                }
            }
        }
    }

    fn zones<'a>(&self, course: &'a Course) -> &'a [Zone] {
        self.bonus
            .and_then(|number| course.bonus(number))
            .map_or(&course.zones, |route| &route.zones)
    }

    fn refresh_board(&mut self, course: &Course) {
        let zones = self.zones(course);
        self.catalog_board = course.board_revision(self.bonus).is_some();
        self.initial_phase = if zones
            .iter()
            .any(|zone| matches!(zone.kind, ZoneKind::Start))
            && zones
                .iter()
                .any(|zone| matches!(zone.kind, ZoneKind::Finish))
        {
            RunPhase::Ready
        } else {
            RunPhase::Untimed
        };
        self.checkpoint_order = zones
            .iter()
            .filter_map(|zone| match zone.kind {
                ZoneKind::Checkpoint(order) => Some(order),
                _ => None,
            })
            .collect();
        self.checkpoint_order.sort_unstable();
        self.checkpoint_order.dedup();
        let stage_starts = &course.map_triggers.authored_stage_starts;
        self.stage_checkpoint_handoffs.clear();
        if self.bonus.is_none()
            && stage_starts.len() > 1
            && stage_starts.len() == self.checkpoint_order.len() + 1
            && stage_starts
                .iter()
                .skip(1)
                .all(|brushes| !brushes.is_empty())
        {
            self.stage_checkpoint_handoffs.extend(self.checkpoint_order.iter().enumerate().map(
                |(index, order)| {
                    zones.iter().filter(|zone| matches!(zone.kind, ZoneKind::Checkpoint(n) if n == *order)).any(|checkpoint| {
                        zones.iter().any(|zone| {
                            matches!(&zone.kind, ZoneKind::AuthoredTeleport(teleport) if teleport.checkpoint && usize::from(teleport.stage.unwrap_or(0)) == index + 2)
                                && zone.brush.min.cmplt(checkpoint.brush.max).all()
                                && zone.brush.max.cmpgt(checkpoint.brush.min).all()
                        })
                    })
                },
            ));
            self.stage_checkpoint_handoffs.push(false);
        }
    }

    fn clear_run(&mut self) {
        self.phase = if self.practice.is_some() {
            RunPhase::Untimed
        } else {
            self.initial_phase
        };
        self.elapsed = 0.0;
        self.splits.clear();
        self.checkpoint_speeds.clear();
        self.run_start_tick = None;
        self.stage_clock = StageClock::default();
        self.sync_matching = 0;
        self.sync_measured = 0;
    }

    fn time_at(&self, at: f64, running_from: f64) -> f64 {
        self.run_start_tick.map_or_else(
            || self.elapsed + (at - running_from) / TICK_HZ,
            |start| (self.triggers.tick.saturating_sub(1) as f64 + at - start) / TICK_HZ,
        )
    }

    fn observe_stage_clock(&mut self, course: &Course, segments: &[MovementSegment], cutoff: f64) {
        let Some(run_start) = self.run_start_tick else {
            return;
        };
        let stage_count = self.stage_checkpoint_handoffs.len();
        if stage_count == 0 {
            return;
        }
        let hull = TriggerHull::for_player(&self.player, self.uses_precise_triggers(course));
        let clock = &mut self.stage_clock;
        let tick = self.triggers.tick as f64;
        let current = self.player.position;
        let complete = self.splits.len() + usize::from(self.phase == RunPhase::Finished);
        while clock.stage_index < stage_count {
            let index = clock.stage_index;
            let stage_end = if index == stage_count - 1 && self.phase == RunPhase::Finished {
                Some(self.elapsed)
            } else {
                self.splits.get(index).copied()
            }
            .map_or(f64::INFINITY, |elapsed| run_start + elapsed * TICK_HZ);
            let mut end_tick = stage_end;
            let mut clamp_after = end_tick;
            if !clock.awaiting_handoff {
                let (main_zones, stage_brushes) = if index == 0 {
                    (course.zones.as_slice(), &[][..])
                } else {
                    (
                        &[][..],
                        course.map_triggers.authored_stage_starts[index].as_slice(),
                    )
                };
                let main_starts = main_zones
                    .iter()
                    .filter_map(|zone| matches!(zone.kind, ZoneKind::Start).then_some(&zone.brush));
                for brush in main_starts.chain(stage_brushes) {
                    for segment in segments.iter().filter(|segment| segment.start <= cutoff) {
                        let sweep_min = segment.from.min(segment.to) + hull.min - Vec3::ONE;
                        let sweep_max = segment.from.max(segment.to) + hull.max + Vec3::ONE;
                        if !hull.precise
                            && (!sweep_min.cmple(brush.max).all()
                                || !sweep_max.cmpge(brush.min).all())
                        {
                            continue;
                        }
                        let end = segment.end.min(cutoff);
                        let fraction = if segment.end == segment.start {
                            1.0
                        } else {
                            (end - segment.start) / (segment.end - segment.start)
                        };
                        let to = (segment.from.as_dvec3()
                            + (segment.to.as_dvec3() - segment.from.as_dvec3()) * fraction)
                            .as_vec3();
                        if hull.interval(to, to, brush).is_some() {
                            continue;
                        }
                        let Some((_, exit)) = hull.interval(segment.from, to, brush) else {
                            continue;
                        };
                        let crossing = tick - 1.0 + segment.start + exit * (end - segment.start);
                        let crossing =
                            if crossing < clock.earliest_start && crossing >= clock.clamp_after {
                                clock.earliest_start
                            } else {
                                crossing
                            };
                        if crossing >= clock.earliest_start
                            && crossing < end_tick
                            && clock.latest_start.is_none_or(|start| crossing > start)
                        {
                            clock.latest_start = Some(crossing);
                        }
                    }
                }
                if index >= complete {
                    break;
                }
                clock.awaiting_handoff = self.stage_checkpoint_handoffs[index];
            }
            if clock.awaiting_handoff {
                let next_starts = &course.map_triggers.authored_stage_starts[index + 1];
                if next_starts
                    .iter()
                    .any(|brush| hull.interval(current, current, brush).is_some())
                {
                    end_tick = tick + 1.0;
                    clamp_after = tick;
                    clock.awaiting_handoff = false;
                } else if complete > index + 1 {
                    clock.latest_start = None;
                    clock.awaiting_handoff = false;
                } else {
                    break;
                }
            }
            if let Some(start_tick) = clock.latest_start {
                let time_us = ((end_tick - start_tick) / TICK_HZ * 1e6).round() as u64;
                if time_us > 0 {
                    clock.results.push(StageResult {
                        stage: (index + 1) as u16,
                        time_us,
                        start_tick,
                        end_tick,
                    });
                }
            }
            clock.stage_index += 1;
            clock.latest_start = None;
            clock.earliest_start = end_tick;
            clock.clamp_after = clamp_after;
        }
    }

    fn cap_start_speed(&mut self, precise: bool) {
        if precise {
            let velocity = self.player.velocity.as_dvec3();
            let speed = velocity.x.hypot(velocity.z);
            let cap = if self.player.grounded { 325.0 } else { 350.0 };
            if speed > cap {
                self.player.velocity.x = (velocity.x * cap / speed) as f32;
                self.player.velocity.z = (velocity.z * cap / speed) as f32;
            }
            return;
        }
        let speed = self.player.velocity.x.hypot(self.player.velocity.z);
        let cap = if self.player.grounded { 325.0 } else { 350.0 };
        if speed > cap {
            let scale = cap / speed;
            self.player.velocity.x *= scale;
            self.player.velocity.z *= scale;
        }
    }

    fn capture_command(&mut self, command: TickCommand) {
        if !matches!(self.phase, RunPhase::Ready | RunPhase::Running) || self.recording_overflow {
            return;
        }
        if let Some(commands) = &mut self.commands {
            if commands.len() < MAX_COMMANDS && command.valid() {
                commands.push(command);
            } else {
                self.recording_overflow = true;
                self.ranked_valid = false;
                commands.clear();
            }
        }
    }

    fn measure_strafe(
        &mut self,
        input: MoveInput,
        inside_start: bool,
        started: bool,
        teleported: bool,
    ) {
        if inside_start {
            self.sync_matching = 0;
            self.sync_measured = 0;
            return;
        }
        if self.player.grounded || started || teleported {
            return;
        }
        let angle = (input.yaw - self.previous_yaw + std::f32::consts::PI)
            .rem_euclid(std::f32::consts::TAU)
            - std::f32::consts::PI;
        let velocity = self.player.velocity.with_y(0.0).normalize_or_zero();
        if angle == 0.0 || !angle.is_finite() || velocity == Vec3::ZERO {
            return;
        }
        let (sin, cos) = input.yaw.sin_cos();
        let relative_angle = velocity
            .dot(Vec3::new(-sin, 0.0, -cos))
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees();
        let matching = if (22.5 < relative_angle && relative_angle < 67.5)
            || (112.5 < relative_angle && relative_angle < 157.5)
        {
            input.forward >= 0.25 || input.strafe >= 0.25
        } else if 67.5 < relative_angle && relative_angle < 112.5 {
            input.forward.abs() >= 0.25
        } else if angle > 0.0 {
            input.strafe <= -0.25
        } else {
            input.strafe >= 0.25
        };
        self.sync_measured = self.sync_measured.saturating_add(1);
        if matching {
            self.sync_matching = self.sync_matching.saturating_add(1);
        }
    }
}

fn start_brush<'a>(
    zones: &'a [Zone],
    kind: &ZoneKind,
    position: Vec3,
    hull: TriggerHull,
) -> Option<&'a Brush> {
    zones
        .iter()
        .find(|zone| {
            let matches = match (&zone.kind, kind) {
                (ZoneKind::Start, ZoneKind::Start) => true,
                (ZoneKind::StageStart(a), ZoneKind::StageStart(b)) => a == b,
                _ => false,
            };
            matches && hull.touches(zone, position)
        })
        .map(|zone| &zone.brush)
}

fn push_in_zones(
    zones: &[Zone],
    player: &Player,
    precise: bool,
    bonus: Option<&BonusRules>,
    accepted: &HashSet<usize>,
) -> Option<Vec3> {
    let position = player.position;
    let hull = TriggerHull::for_player(player, precise);
    let mut visited = HashSet::new();
    let mut push = None;
    for zone in zones {
        if let ZoneKind::Push { index, velocity } = zone.kind
            && (bonus.is_none_or(|rules| !rules.push_filters.contains_key(&index))
                || accepted.contains(&index))
            && hull.touches(zone, position)
            && visited.insert(index)
        {
            *push.get_or_insert(Vec3::ZERO) += velocity;
        }
    }
    push
}

#[derive(Clone, Copy)]
enum ZoneEvent<'a> {
    StartEntry,
    StartExit,
    Checkpoint(u16),
    Finish,
    Reset,
    Teleport {
        index: usize,
        position: Vec3,
        yaw: Option<f32>,
        clear_velocity: bool,
    },
    AuthoredTeleport(&'a AuthoredTeleport),
}

impl ZoneEvent<'_> {
    fn order(self) -> (u8, u16) {
        match self {
            Self::StartEntry => (0, 0),
            Self::StartExit => (1, 0),
            Self::Checkpoint(order) => (2, order),
            Self::Finish => (3, 0),
            Self::Reset => (4, 0),
            Self::Teleport { .. } | Self::AuthoredTeleport(_) => (5, 0),
        }
    }
}

fn zone_events<'a>(
    zones: &'a [Zone],
    segments: &[MovementSegment],
    from: Vec3,
    to: Vec3,
    hull: TriggerHull,
    targetname: &str,
    bonus_rules: bool,
) -> Vec<(f64, ZoneEvent<'a>)> {
    let mut events = Vec::new();
    let mut starts = Vec::new();
    let fallback = [MovementSegment {
        from,
        to,
        start: 0.0,
        end: 1.0,
    }];
    let has_motion = !segments.is_empty();
    let segments = if has_motion { segments } else { &fallback };
    for segment in segments {
        let sweep_min = segment.from.min(segment.to) + hull.min;
        let sweep_max = segment.from.max(segment.to) + hull.max;
        for (index, zone) in zones.iter().enumerate() {
            if !hull.precise
                && (!sweep_min.cmple(zone.brush.max).all()
                    || !sweep_max.cmpge(zone.brush.min).all())
            {
                continue;
            }
            let event = match zone.kind {
                ZoneKind::StageStart(_)
                | ZoneKind::Push { .. }
                | ZoneKind::NoJump(_)
                | ZoneKind::PlayerOutput(_) => continue,
                ZoneKind::Start => None,
                ZoneKind::Checkpoint(order) => Some(ZoneEvent::Checkpoint(order)),
                ZoneKind::Finish => Some(ZoneEvent::Finish),
                ZoneKind::Reset => Some(ZoneEvent::Reset),
                ZoneKind::Teleport { position, yaw }
                | ZoneKind::StageTeleport { position, yaw } => Some(ZoneEvent::Teleport {
                    index,
                    position,
                    yaw,
                    clear_velocity: matches!(zone.kind, ZoneKind::StageTeleport { .. }),
                }),
                ZoneKind::AuthoredTeleport(ref teleport) => {
                    let allowed = teleport
                        .filter_multi
                        .as_ref()
                        .filter(|_| bonus_rules)
                        .map_or_else(
                            || {
                                teleport
                                    .filter
                                    .as_ref()
                                    .is_none_or(|filter| filter.accepts(targetname))
                            },
                            |filter| filter.accepts(targetname),
                        );
                    if !has_motion || !allowed {
                        continue;
                    }
                    Some(ZoneEvent::AuthoredTeleport(teleport))
                }
            };
            let Some((enter, exit)) = hull.interval(segment.from, segment.to, &zone.brush) else {
                continue;
            };
            let duration = segment.end - segment.start;
            let enter = segment.start + enter * duration;
            let exit = segment.start + exit * duration;
            if let Some(event) = event {
                events.push((enter, event));
            } else {
                starts.push((enter, exit));
            }
        }
    }

    // Multiple start brushes describe a single trigger. Crossing an overlap keeps it armed.
    starts.sort_by(|left, right| left.0.total_cmp(&right.0));
    let mut starts = starts.into_iter().peekable();
    let merge_slop = if hull.precise { 1e-9 } else { 1e-7 };
    while let Some((enter, mut exit)) = starts.next() {
        while let Some((_, next_exit)) =
            starts.next_if(|(next_enter, _)| *next_enter <= exit + merge_slop)
        {
            exit = exit.max(next_exit);
        }
        if starts.peek().is_none() && start_brush(zones, &ZoneKind::Start, to, hull).is_some() {
            exit = 1.0;
        }
        events.push((enter, ZoneEvent::StartEntry));
        if exit < 1.0 {
            events.push((exit, ZoneEvent::StartExit));
        }
    }
    events.sort_by(|(a, event_a), (b, event_b)| {
        a.total_cmp(b)
            .then_with(|| event_a.order().cmp(&event_b.order()))
    });
    events
}

#[derive(Clone, Copy)]
struct TriggerHull {
    min: Vec3,
    max: Vec3,
    precise: bool,
}

impl TriggerHull {
    fn for_player(player: &Player, precise: bool) -> Self {
        let (min, max) = player.hull_bounds();
        Self {
            min: min - player.position,
            max: max - player.position,
            precise,
        }
    }

    fn touches(self, zone: &Zone, position: Vec3) -> bool {
        (self.precise
            || ((position + self.min).cmple(zone.brush.max).all()
                && (position + self.max).cmpge(zone.brush.min).all()))
            && self.interval(position, position, &zone.brush).is_some()
    }

    fn interval(self, from: Vec3, to: Vec3, brush: &Brush) -> Option<(f64, f64)> {
        if self.precise
            && let Some(geometry) = brush.planes.zone_geometry()
        {
            return self.authored_interval(from.as_dvec3(), to.as_dvec3(), geometry);
        }
        zone_interval(from, to, self.min, self.max, brush)
    }

    fn authored_interval(
        self,
        from: DVec3,
        to: DVec3,
        geometry: &crate::course::AuthoredZoneGeometry,
    ) -> Option<(f64, f64)> {
        use crate::course::source_dot;

        let hull_min = self.min.as_dvec3();
        let hull_max = self.max.as_dvec3();
        let lower = geometry.min - hull_max;
        let upper = geometry.max - hull_min;
        if from.max(to).cmplt(lower).any() || from.min(to).cmpgt(upper).any() {
            return None;
        }
        let delta = to - from;
        let mut enter = 0.0_f64;
        let mut exit = 1.0_f64;
        for axis in 0..3 {
            if delta[axis].abs() < 1e-12 {
                if from[axis] < lower[axis] || from[axis] > upper[axis] {
                    return None;
                }
                continue;
            }
            let first = (lower[axis] - from[axis]) / delta[axis];
            let last = (upper[axis] - from[axis]) / delta[axis];
            enter = enter.max(first.min(last));
            exit = exit.min(first.max(last));
            if enter > exit {
                return None;
            }
        }
        if geometry.planes.is_empty() {
            return Some((enter, exit));
        }
        (enter, exit) = (0.0, 1.0);
        for &(normal, distance) in &geometry.planes {
            let support = DVec3::select(normal.cmplt(DVec3::ZERO), hull_max, hull_min);
            let offset = source_dot(normal, support);
            let before = source_dot(from, normal) - distance + offset;
            let after = source_dot(to, normal) - distance + offset;
            if before > 0.0 && after > 0.0 {
                return None;
            }
            if before > 0.0 {
                enter = enter.max(before / (before - after));
            } else if after > 0.0 {
                exit = exit.min(before / (before - after));
            }
            if enter > exit {
                return None;
            }
        }
        Some((enter, exit))
    }
}

fn zone_interval(
    from: Vec3,
    to: Vec3,
    hull_min: Vec3,
    hull_max: Vec3,
    brush: &Brush,
) -> Option<(f64, f64)> {
    use crate::course::source_dot;

    let mut enter = 0.0_f64;
    let mut exit = 1.0_f64;
    for plane in &brush.planes {
        let support = Vec3::select(plane.normal.cmplt(Vec3::ZERO), hull_max, hull_min);
        let normal = plane.normal.as_dvec3();
        let distance = f64::from(plane.distance) - source_dot(normal, support.as_dvec3());
        let before = source_dot(normal, from.as_dvec3()) - distance;
        let after = source_dot(normal, to.as_dvec3()) - distance;
        if before > 0.0 && after > 0.0 {
            return None;
        }
        if before > 0.0 {
            enter = enter.max(before / (before - after));
        } else if after > 0.0 {
            exit = exit.min(before / (before - after));
        }
        if enter > exit {
            return None;
        }
    }
    Some((enter, exit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::course::Zone;

    fn zone(min_x: f32, max_x: f32, kind: ZoneKind) -> Zone {
        Zone {
            brush: Brush::box_at(
                Vec3::new(min_x, -100.0, -100.0),
                Vec3::new(max_x, 100.0, 100.0),
            ),
            kind,
        }
    }

    fn course(zones: Vec<Zone>) -> Course {
        Course {
            name: "Test".to_owned(),
            catalog_revision: Some("test-course-v1".to_owned()),
            bonus_routes: Vec::new(),
            map_triggers: Default::default(),
            spawn: Vec3::ZERO,
            spawn_yaw: 1.25,
            brushes: Vec::new(),
            surfaces: Vec::new(),
            textures: Vec::new(),
            visual_meshes: Vec::new(),
            visual_visibility: None,
            practice_spawns: Vec::new(),
            sky: None,
            sky_occluders: Vec::new(),
            fog: None,
            zones,
            kill_y: -1000.0,
        }
    }

    #[test]
    fn staged_course_times_each_stage_from_its_own_start_exit() {
        let mut course = course(vec![
            zone(-10.0, 10.0, ZoneKind::Start),
            zone(90.0, 100.0, ZoneKind::Checkpoint(1)),
            zone(220.0, 230.0, ZoneKind::Finish),
        ]);
        course.map_triggers.continuous_stages = true;
        course.map_triggers.authored_stage_starts = vec![
            vec![],
            vec![zone(120.0, 130.0, ZoneKind::StageStart(2)).brush],
        ];
        let mut session = Session::with_options(
            &course,
            RunOptions {
                unrestricted_start: true,
                ..Default::default()
            },
        );
        session.player.velocity.x = 2500.0;
        for _ in 0..6 {
            session.tick(&course, MoveInput::default());
        }

        let [first, second] = session.verified_stages() else {
            panic!("the complete run should contain two verified stages");
        };
        assert_eq!(first.stage, 1);
        assert_eq!(second.stage, 2);
        assert_eq!(first.time_us, 19_200);
        assert_eq!(second.time_us, 23_200);
        assert!(first.end_tick < second.start_tick);
        assert!((session.elapsed - 0.0712).abs() < 1e-7);
    }

    #[test]
    fn stage_restart_keeps_run_clock_through_the_first_start_zone() {
        let mut course = course(vec![
            zone(-10.0, 10.0, ZoneKind::Start),
            zone(120.0, 130.0, ZoneKind::Finish),
        ]);
        course
            .map_triggers
            .authored_stage_spawns
            .push(PracticeSpawn {
                name: "Stage 1".to_owned(),
                position: Vec3::ZERO,
                yaw: 0.3,
                pitch: -0.4,
                source_yaw_degrees: Some(107.188734536),
            });
        let mut session = Session::with_options(
            &course,
            RunOptions {
                unrestricted_start: true,
                ..Default::default()
            },
        );
        session.player.velocity.x = 2500.0;
        session.tick(&course, MoveInput::default());
        let before = session.elapsed;
        assert_eq!(
            session.tick_command(
                &course,
                TickCommand {
                    input: MoveInput::default(),
                    pitch: 0.0,
                    restart_stage: true,
                    source_yaw_degrees: None,
                }
            ),
            Some(0.3)
        );
        assert_eq!(session.pitch_override(), Some(-0.4));
        assert_eq!(session.source_yaw_override(), Some(107.188734536));
        assert_eq!(session.player.position, Vec3::ZERO);
        assert!((session.elapsed - before - TICK_HZ.recip()).abs() < 1e-9);

        session.player.velocity.x = 2500.0;
        for _ in 0..3 {
            session.tick(&course, MoveInput::default());
        }
        assert_eq!(session.phase, RunPhase::Finished);
        assert!(session.elapsed > 4.0 / TICK_HZ);
        assert!(session.take_finished_replay(&course).unwrap().commands[1].restart_stage);
    }

    #[test]
    fn fast_run_crosses_thin_zones_in_order_within_a_tick() {
        let course = course(vec![
            zone(68.0, 69.0, ZoneKind::Finish),
            zone(57.0, 58.0, ZoneKind::Checkpoint(20)),
            zone(-8.0, 8.0, ZoneKind::Start),
            zone(41.0, 42.0, ZoneKind::Checkpoint(10)),
        ]);
        let mut session = Session::new(&course);
        session.player.velocity = Vec3::X * 4000.0;
        session.tick(&course, MoveInput::default());

        assert_eq!(session.phase, RunPhase::Finished);
        assert_eq!(session.splits.len(), 2);
        assert!((session.elapsed - 0.007).abs() < 0.00001);
        assert_eq!(session.best, Some(session.elapsed));
        assert!(session.player.velocity.x <= 350.0);

        session.restart(&course);
        assert_eq!(session.phase, RunPhase::Ready);
        assert!(session.splits.is_empty());
        assert!(session.best.is_some());
    }

    #[test]
    fn crossing_finish_before_required_checkpoint_keeps_the_run_active() {
        let course = course(vec![
            zone(-8.0, 8.0, ZoneKind::Start),
            zone(41.0, 42.0, ZoneKind::Finish),
            zone(68.0, 69.0, ZoneKind::Checkpoint(1)),
        ]);
        let mut session = Session::new(&course);
        session.player.velocity = Vec3::X * 4000.0;
        session.tick(&course, MoveInput::default());

        assert_eq!(session.phase, RunPhase::Running);
        assert_eq!(session.splits.len(), 1);
        assert_eq!(session.best, None);
    }

    #[test]
    fn untimed_map_supports_teleports_and_fall_restarts() {
        let target = Vec3::new(100.0, 50.0, 100.0);
        let course = course(vec![zone(
            41.0,
            42.0,
            ZoneKind::Teleport {
                position: target,
                yaw: Some(0.5),
            },
        )]);
        let mut session = Session::new(&course);
        session.player.velocity = Vec3::X * 4000.0;

        assert_eq!(session.tick(&course, MoveInput::default()), Some(0.5));
        assert_eq!(session.player.position, target);
        assert_eq!(session.player.velocity.x, 4000.0);
        assert_eq!(session.phase, RunPhase::Untimed);
        let teleport_revision = session.pose_revision();
        assert_ne!(teleport_revision, 0);
        session.tick(&course, MoveInput::default());
        assert_eq!(session.pose_revision(), teleport_revision);

        session.player.position.y = course.kill_y - 1.0;
        assert_eq!(session.tick(&course, MoveInput::default()), Some(1.25));
        assert_eq!(session.player.position, course.spawn);
        assert_eq!(session.player.velocity, Vec3::ZERO);
        assert_ne!(session.pose_revision(), teleport_revision);
    }

    #[test]
    fn stage_transition_clears_velocity_without_restarting_the_timer() {
        let target = Vec3::new(100.0, 50.0, 100.0);
        let course = course(vec![
            zone(-8.0, 8.0, ZoneKind::Start),
            zone(41.0, 42.0, ZoneKind::Checkpoint(1)),
            zone(
                41.0,
                42.0,
                ZoneKind::StageTeleport {
                    position: target,
                    yaw: Some(0.75),
                },
            ),
            zone(150.0, 160.0, ZoneKind::Finish),
        ]);
        let mut session = Session::new(&course);
        session.player.velocity = Vec3::X * 4000.0;

        assert_eq!(session.tick(&course, MoveInput::default()), Some(0.75));
        assert_eq!(session.player.position, target);
        assert_eq!(session.player.velocity, Vec3::ZERO);
        assert_eq!(session.phase, RunPhase::Running);
        assert_eq!(session.splits.len(), 1);
        assert!(session.elapsed > session.splits[0]);
    }

    #[test]
    fn selected_practice_spawn_stays_untimed_and_resets_at_its_own_position() {
        let course = course(vec![
            zone(-8.0, 8.0, ZoneKind::Start),
            zone(41.0, 42.0, ZoneKind::Finish),
        ]);
        let spawn = PracticeSpawn {
            name: "Stage 2".to_owned(),
            position: Vec3::new(70.0, 10.0, 90.0),
            yaw: 0.75,
            pitch: -0.19,
            source_yaw_degrees: None,
        };
        let mut session = Session::new(&course);
        session.best = Some(4.0);
        session.set_practice(&course, Some(&spawn));
        assert_eq!(session.phase, RunPhase::Untimed);
        assert_eq!(session.player.position, spawn.position);
        session.player.position.y = course.kill_y - 100.0;
        assert_eq!(session.tick(&course, MoveInput::default()), Some(spawn.yaw));
        assert_eq!(session.player.position, spawn.position);
        assert_eq!(session.phase, RunPhase::Untimed);

        session.set_practice(&course, None);
        assert_eq!(session.phase, RunPhase::Ready);
        assert_eq!(session.player.position, course.spawn);
        assert_eq!(session.spawn_yaw(&course), course.spawn_yaw);
        assert_eq!(session.best, Some(4.0));
    }

    #[test]
    fn capped_start_allows_one_hop_then_rearms_after_settling() {
        let mut course = course(vec![
            zone(-100.0, 100.0, ZoneKind::Start),
            zone(140.0, 150.0, ZoneKind::Finish),
        ]);
        course.spawn = Vec3::Y * 0.05;
        course.brushes.push(Brush::box_at(
            Vec3::new(-1000.0, -50.0, -1000.0),
            Vec3::new(1000.0, 0.0, 1000.0),
        ));
        let jump = MoveInput {
            jump: true,
            ..Default::default()
        };
        let mut session = Session::new(&course);
        session.tick(&course, jump);
        assert!(session.player.velocity.y > 250.0);
        for _ in 0..100 {
            session.tick(&course, jump);
        }
        assert!(session.player.grounded);
        assert_eq!(session.phase, RunPhase::Ready);
        assert_eq!(session.start_jumps, 1);
        for _ in 0..7 {
            session.tick(&course, MoveInput::default());
        }
        session.tick(&course, jump);
        assert!(session.player.velocity.y > 250.0);
    }

    #[test]
    fn restoring_a_saved_pose_enters_practice_without_rewriting_the_live_run() {
        let course = course(vec![
            zone(-8.0, 8.0, ZoneKind::Start),
            zone(68.0, 69.0, ZoneKind::Finish),
        ]);
        let mut session = Session::new(&course);
        session.player.velocity = Vec3::Z * 32.0;
        assert!(session.save_position(0.25, -0.5));
        assert!(session.ranked_eligible());
        session.mark_unranked();
        assert!(!session.ranked_eligible());
        session.restart(&course);
        assert!(session.ranked_eligible());
        assert_eq!(session.restore_position(&course), Some((0.25, -0.5)));
        assert_eq!(session.player.velocity, Vec3::Z * 32.0);
        assert_eq!(session.phase, RunPhase::Untimed);
        assert!(!session.ranked_eligible());
    }

    #[test]
    fn output_enables_a_filtered_landmark_teleport_with_original_momentum() {
        use crate::course::{PlayerOutputZone, TriggerFilter};
        let course = course(vec![
            zone(
                20.0,
                75.0,
                ZoneKind::PlayerOutput(Box::new(PlayerOutputZone {
                    index: 0,
                    filter: None,
                    filter_multi: None,
                    on_touch: Vec::new(),
                    on_start: vec![PlayerOutput {
                        delay_ticks: 0,
                        value: PlayerOutputValue::TargetName("GATE_selected".into()),
                    }],
                    on_end: Vec::new(),
                })),
            ),
            zone(
                41.0,
                42.0,
                ZoneKind::AuthoredTeleport(Box::new(AuthoredTeleport {
                    position: Vec3::new(500.0, 20.0, 0.0),
                    yaw: 2.0,
                    pitch: -0.4,
                    source_yaw_degrees: None,
                    landmark: Some(Vec3::X * 40.0),
                    relative: false,
                    keep_view: false,
                    stage: None,
                    stage_velocity: false,
                    preserve_velocity: true,
                    checkpoint: false,
                    restart: false,
                    filter: Some(TriggerFilter {
                        targetname: "gate_*".into(),
                        negated: false,
                    }),
                    filter_multi: None,
                    carry_push: false,
                })),
            ),
        ]);
        let mut session = Session::new(&course);
        session.player.velocity = Vec3::X * 4000.0;
        assert_eq!(session.tick(&course, MoveInput::default()), None);
        assert_eq!(session.pitch_override(), None);
        assert!((session.player.position.x - 520.0).abs() < 0.001);
        assert_eq!(session.player.velocity.x, 4000.0);
    }

    #[test]
    fn authored_stage_handoff_uses_next_tick_and_destination_view() {
        let destination = Vec3::new(200.0, 50.0, 0.0);
        let mut course = course(vec![
            zone(-8.0, 8.0, ZoneKind::Start),
            zone(41.0, 42.0, ZoneKind::Checkpoint(1)),
            zone(
                41.0,
                42.0,
                ZoneKind::AuthoredTeleport(Box::new(AuthoredTeleport {
                    position: destination,
                    yaw: 2.0,
                    pitch: -0.4,
                    source_yaw_degrees: Some(204.591559026),
                    landmark: None,
                    relative: false,
                    keep_view: false,
                    stage: Some(2),
                    stage_velocity: true,
                    preserve_velocity: false,
                    checkpoint: true,
                    restart: false,
                    filter: None,
                    filter_multi: None,
                    carry_push: false,
                })),
            ),
            zone(450.0, 460.0, ZoneKind::Finish),
        ]);
        course.map_triggers.authored_stage_starts = vec![
            vec![],
            vec![zone(190.0, 220.0, ZoneKind::StageStart(2)).brush],
        ];
        let mut session = Session::new(&course);
        session.player.velocity = Vec3::X * 4000.0;
        assert_eq!(session.tick(&course, MoveInput::default()), None);
        assert_ne!(session.player.position, destination);
        assert_eq!(session.splits.len(), 1);
        let elapsed = session.elapsed;
        assert_eq!(session.tick(&course, MoveInput::default()), Some(2.0));
        assert_eq!(session.pitch_override(), Some(-0.4));
        assert_eq!(session.source_yaw_override(), Some(204.591559026));
        assert_eq!(session.player.position, destination);
        assert_eq!(session.player.velocity, Vec3::NEG_Y * HALF_GRAVITY_STEP);
        assert_eq!(session.triggers.stage, 2);
        assert!(session.elapsed > elapsed);

        session.player.velocity.x = 4000.0;
        for _ in 0..4 {
            session.tick(&course, MoveInput::default());
        }
        let [first, second] = session.verified_stages() else {
            panic!("both stages should be timed across the handoff");
        };
        assert_eq!(first.end_tick, 3.0);
        assert_eq!(second.start_tick, first.end_tick);
    }

    #[test]
    fn push_displaces_without_accumulating_until_exit_and_no_jump_scales_movement() {
        let mut course = course(vec![
            zone(-90.0, 90.0, ZoneKind::NoJump(0.5)),
            zone(
                -90.0,
                90.0,
                ZoneKind::Push {
                    index: 0,
                    velocity: Vec3::X * 100.0,
                },
            ),
            zone(
                -80.0,
                80.0,
                ZoneKind::Push {
                    index: 0,
                    velocity: Vec3::X * 100.0,
                },
            ),
        ]);
        course.spawn = Vec3::Y * 0.05;
        course.brushes.push(Brush::box_at(
            Vec3::new(-1000.0, -50.0, -1000.0),
            Vec3::new(1000.0, 0.0, 1000.0),
        ));
        let mut session = Session::new(&course);
        let jump = MoveInput {
            jump: true,
            ..Default::default()
        };
        session.tick(&course, jump);
        assert!(session.player.grounded);
        assert!((session.player.position.x - 0.75).abs() < 0.0001);
        assert_eq!(session.player.velocity.x, 0.0);
        session.player.position.x = 200.0;
        session.tick(&course, MoveInput::default());
        assert!(session.player.velocity.x > 90.0);
    }
}
