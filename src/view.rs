use std::time::Duration;

use bevy::input::InputSystems;
use bevy::input::mouse::{AccumulatedMouseScroll, MouseMotion};
use bevy::prelude::*;
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Tonemapping};
use bevy::render::{renderer::RenderDevice, settings::WgpuFeatures};
use bevy::window::{CursorGrabMode, CursorMoved, CursorOptions, PrimaryWindow};
use bevy::winit::{UpdateMode, WinitSettings};

use bevy_surf::{
    course::Course,
    look::{CursorMotion, SourceLook},
    movement::{MoveInput, TICK_HZ, TICK_SECONDS, native_yaw_to_source},
    reference_ghost::{ReferenceGhost, ReferenceReplayFrame},
    replay::{NativeGhost, NativeGhostPreparation, Playback, ReplayError, RunReplay, TickCommand},
    session::{RunOptions, RunPhase, Session},
    tutorial::{self, LessonSection},
};

use crate::bench::{self, BenchRequest, FrameBenchmark};
use crate::library::{LoadEvent, LoadJob, LoadedCourse, MapLibrary};
use crate::menu::{self, ControlAction, MenuState, Page, Settings};
use crate::online::{self, Online};
use crate::world;

#[cfg(windows)]
#[path = "render_compat.rs"]
mod render_compat;

#[path = "lesson_drill_feedback.rs"]
mod lesson_drill_feedback;
use lesson_drill_feedback::DrillTrace;

#[path = "lesson_live_coach.rs"]
mod lesson_live_coach;
pub(super) use lesson_drill_feedback::{DrillFeedback, DrillFinding};
use lesson_live_coach::LiveLessonCoach;
pub(super) use lesson_live_coach::LiveLessonCue;

pub const BACKGROUND: Color = Color::srgb(0.075, 0.135, 0.17);
const GHOST_PREPARATION_BUDGET: Duration = Duration::from_millis(1);
const GHOST_PREPARATION_COMMANDS: usize = 1024;

pub enum StartupReplay {
    Native(RunReplay),
    Reference(ReferenceGhost),
}

struct ReferencePlayback {
    track: ReferenceGhost,
    session: Session,
    frame: ReferenceReplayFrame,
    cursor: usize,
    accumulator: f64,
}

struct PendingGhost {
    preparation: NativeGhostPreparation,
    bonus: Option<u16>,
    restart: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum DrillResult {
    Ready,
    Missed(DrillFeedback),
    Cleared(f64),
}

struct LessonDrill {
    section: usize,
    start: f64,
    entry: Vec<TickCommand>,
    result: Option<DrillResult>,
    return_session: Box<Session>,
    return_look: SourceLook,
    return_page: Page,
    return_coach: Option<LiveLessonCoach>,
    trace: DrillTrace,
}

impl ReferencePlayback {
    fn new(course: &Course, track: ReferenceGhost) -> Result<Self, String> {
        if !track.matches_course(course) {
            return Err("The reference replay belongs to a different course revision".into());
        }
        let frame = track
            .replay_frame(0)
            .ok_or("Reference replay is missing first-person data")?;
        let mut session = Session::new(course);
        if let Some(bonus) = track.bonus() {
            session.select_bonus(course, Some(bonus))?;
        }
        let mut playback = Self {
            track,
            session,
            frame,
            cursor: 0,
            accumulator: 0.0,
        };
        playback.seek(0);
        Ok(playback)
    }

    fn seek(&mut self, cursor: usize) -> bool {
        let Some(frame) = self.track.replay_frame(cursor) else {
            return false;
        };
        self.cursor = cursor;
        self.frame = frame;
        self.session.player.position = frame.pose.position;
        self.session.player.velocity = frame.velocity;
        self.session.elapsed = self.track.elapsed_at(cursor);
        self.session.phase = if cursor + 1 == self.track.sample_count() {
            RunPhase::Finished
        } else if self.track.started_at(cursor) {
            RunPhase::Running
        } else {
            RunPhase::Ready
        };
        if let Some((splits, speeds)) = self.track.recorded_splits() {
            let visible = splits.partition_point(|split| *split <= self.session.elapsed);
            self.session.splits.clear();
            self.session.splits.extend_from_slice(&splits[..visible]);
            self.session.checkpoint_speeds.clear();
            self.session
                .checkpoint_speeds
                .extend_from_slice(&speeds[..visible]);
        }
        true
    }

    fn advance(&mut self) -> bool {
        self.accumulator += 1.0 / TICK_HZ;
        while self.accumulator >= self.track.tick_seconds() {
            self.accumulator -= self.track.tick_seconds();
            if !self.seek(self.cursor + 1) {
                return false;
            }
        }
        true
    }
}

#[derive(Resource)]
pub struct Game {
    pub course: Course,
    pub session: Session,
    pub course_version: u64,
    pub practice_name: Option<String>,
    pub lesson_retries: u32,
    lesson: Option<&'static bevy_surf::tutorial::Lesson>,
    lesson_drill: Option<LessonDrill>,
    lesson_coach: Option<LiveLessonCoach>,
    starter_guide: bool,
    pub run_epoch: u64,
    pub signed_at_finish: bool,
    pub replay_epoch: Option<u64>,
    last_replay: Option<RunReplay>,
    playback: Option<Playback>,
    reference_playback: Option<ReferencePlayback>,
    replay_paused: bool,
    ghost: Option<NativeGhost>,
    ghost_preparation: Option<PendingGhost>,
    reference_ghost: Option<ReferenceGhost>,
    selected_bonus: Option<u16>,
    look: SourceLook,
    third_person: bool,
    previous_eye: Vec3,
    turn_rate: f64,
    jump_pending: bool,
    stage_restart_pending: bool,
    pub input: MoveInput,
    pub pending: Option<LoadJob>,
}

impl Game {
    fn start_replay(&mut self, replay: StartupReplay) -> Result<(), String> {
        match replay {
            StartupReplay::Native(replay) => self.play_replay(replay, false),
            StartupReplay::Reference(track) => self.play_reference_replay(track),
        }
    }

    fn seek_benchmark_replay(&mut self, tick: usize, hold_tick: bool) -> Result<(), String> {
        if let Some(playback) = &mut self.reference_playback {
            if !playback.seek(tick) {
                return Err("Benchmark start tick is outside the replay".into());
            }
        } else if let Some(playback) = &mut self.playback {
            if tick > playback.len() {
                return Err("Benchmark start tick is outside the replay".into());
            }
            while playback.cursor < tick {
                playback.tick(&self.course);
            }
        }
        self.replay_paused = hold_tick;
        Ok(())
    }

    pub(super) fn new(course: Course) -> Self {
        let session = Session::new(&course);
        Self::from_loaded(LoadedCourse {
            course,
            session,
            lesson: None,
            starter_guide: false,
        })
    }

    fn from_loaded(
        LoadedCourse {
            course,
            session,
            lesson,
            starter_guide,
        }: LoadedCourse,
    ) -> Self {
        let look = SourceLook::new(
            session.spawn_source_yaw(&course),
            session.spawn_pitch(&course),
        );
        let previous_eye = session.player.position + Vec3::Y * session.player.eye_height();
        let lesson_coach = lesson.and_then(|_| LiveLessonCoach::for_course(&course));
        Self {
            course,
            session,
            course_version: 0,
            practice_name: None,
            lesson_retries: 0,
            lesson,
            lesson_drill: None,
            lesson_coach,
            starter_guide,
            run_epoch: 1,
            signed_at_finish: false,
            replay_epoch: None,
            last_replay: None,
            playback: None,
            reference_playback: None,
            replay_paused: false,
            ghost: None,
            ghost_preparation: None,
            reference_ghost: None,
            selected_bonus: None,
            look,
            third_person: false,
            previous_eye,
            turn_rate: 0.0,
            jump_pending: false,
            stage_restart_pending: false,
            input: MoveInput::default(),
            pending: None,
        }
    }

    fn eye(&self) -> Vec3 {
        self.session.player.position + Vec3::Y * self.session.player.eye_height()
    }

    pub fn restart(&mut self) {
        if self.restart_lesson_drill() {
            return;
        }
        self.session.restart(&self.course);
        self.lesson_retries = 0;
        self.new_run();
        self.reset_view();
    }

    pub fn set_run_options(&mut self, options: RunOptions) {
        let options = if self.tutorial_lesson().is_some() {
            RunOptions::default()
        } else {
            options
        };
        if self.session.options() == options {
            return;
        }
        self.session.set_options(&self.course, options);
        self.new_run();
        self.reset_view();
    }

    pub fn set_practice(&mut self, index: Option<usize>) {
        if self.lesson_drill.is_some() {
            return;
        }
        let spawn = index.and_then(|index| self.course.practice_spawns.get(index));
        self.practice_name = spawn.map(|spawn| spawn.name.clone());
        self.session.set_practice(&self.course, spawn);
        self.new_run();
        self.reset_view();
    }

    pub fn save_position(&mut self) {
        if self.lesson_drill.is_some() {
            return;
        }
        let (yaw, pitch) = self.look.native();
        self.session
            .save_position_with_source_yaw(yaw, pitch, Some(self.look.yaw));
    }

    pub fn restore_position(&mut self) -> bool {
        if self.lesson_drill.is_some() {
            return false;
        }
        let Some((yaw, pitch)) = self.session.restore_position(&self.course) else {
            return false;
        };
        self.new_run();
        self.practice_name = Some("Saved position".into());
        let source_yaw = self
            .session
            .source_yaw_override()
            .unwrap_or_else(|| native_yaw_to_source(yaw));
        self.look = SourceLook::new(source_yaw, pitch);
        self.previous_eye = self.eye();
        true
    }

    pub fn set_bonus(&mut self, number: Option<u16>) -> bool {
        if self.lesson_drill.is_some() {
            return false;
        }
        if self.session.select_bonus(&self.course, number).is_err() {
            return false;
        }
        if self.selected_bonus != number {
            self.clear_ghost();
        }
        self.selected_bonus = number;
        self.practice_name = None;
        self.new_run();
        self.reset_view();
        true
    }

    pub fn selected_bonus(&self) -> Option<u16> {
        self.selected_bonus
    }
    pub fn tutorial_lesson(&self) -> Option<&'static bevy_surf::tutorial::Lesson> {
        self.lesson
    }

    pub(super) fn drill_section(&self) -> Option<&'static LessonSection> {
        self.lesson?
            .sections
            .get(self.lesson_drill.as_ref()?.section)
    }

    pub(super) fn drill_result(&self) -> Option<DrillResult> {
        self.lesson_drill.as_ref()?.result
    }

    pub(super) fn has_live_lesson_coach(&self) -> bool {
        self.lesson_coach.is_some()
    }

    pub(super) fn live_lesson_cue(&self) -> Option<LiveLessonCue> {
        let coach = self.lesson_coach.as_ref()?;
        if self.watching_replay()
            || self.session.phase != RunPhase::Running
            || !coach.supports_section(self.session.splits.len())
        {
            return None;
        }
        coach.cue
    }

    pub(super) fn practice_lesson_section(
        &mut self,
        index: usize,
        return_page: Page,
    ) -> Result<(), &'static str> {
        let can_practice = match return_page {
            Page::Finished => self.session.phase == RunPhase::Finished,
            Page::Pause => {
                matches!(self.session.phase, RunPhase::Ready | RunPhase::Running)
                    && index == self.session.splits.len()
            }
            _ => false,
        };
        if self
            .lesson
            .is_none_or(|lesson| index >= lesson.sections.len())
            || !can_practice
            || self.lesson_drill.is_some()
            || self.watching_replay()
        {
            return Err("Pause an active lesson or choose a section from your results.");
        }
        let mut demo = tutorial::demonstration(&self.course)
            .ok_or("The demonstration for this lesson could not be prepared.")?;
        let mut session = Session::new(&self.course);
        let entry = demo
            .commands
            .iter()
            .position(|&command| {
                session.tick_command(&self.course, command);
                session.phase == RunPhase::Running && session.splits.len() == index
            })
            .ok_or("The demonstration does not reach this section.")?;
        demo.commands.truncate(entry + 1);
        let start = session.elapsed;
        let trace = DrillTrace::new(&self.course, index, demo.commands[entry], &session.player);
        session.mark_unranked();
        let return_session = Box::new(std::mem::replace(&mut self.session, session));
        let return_look = self.look;
        let return_coach = self.lesson_coach;
        self.lesson_coach = self
            .lesson_coach
            .filter(|coach| coach.supports_section(index));
        if let Some(coach) = &mut self.lesson_coach {
            coach.reset();
        }
        self.use_drill_entry_view(demo.commands[entry]);
        self.lesson_drill = Some(LessonDrill {
            section: index,
            start,
            entry: demo.commands,
            result: Some(DrillResult::Ready),
            return_session,
            return_look,
            return_page,
            return_coach,
            trace,
        });
        Ok(())
    }

    fn restart_lesson_drill(&mut self) -> bool {
        let Some(drill) = &mut self.lesson_drill else {
            return false;
        };
        self.session.restart(&self.course);
        for &command in &drill.entry {
            self.session.tick_command(&self.course, command);
        }
        self.session.mark_unranked();
        drill.result = None;
        let command = *drill.entry.last().expect("A drill has an entry command");
        drill.trace = DrillTrace::new(&self.course, drill.section, command, &self.session.player);
        if let Some(coach) = &mut self.lesson_coach {
            coach.reset();
        }
        self.use_drill_entry_view(command);
        true
    }

    fn use_drill_entry_view(&mut self, command: TickCommand) {
        let yaw = command
            .source_yaw_degrees
            .unwrap_or_else(|| native_yaw_to_source(command.input.yaw));
        self.set_drill_view(SourceLook::new(yaw, command.pitch));
    }

    fn set_drill_view(&mut self, look: SourceLook) {
        self.look = look;
        self.previous_eye = self.eye();
        self.input = MoveInput::default();
        self.turn_rate = 0.0;
        self.jump_pending = false;
        self.stage_restart_pending = false;
    }

    pub(super) fn exit_lesson_drill(&mut self) -> Option<Page> {
        let drill = self.lesson_drill.take()?;
        self.session = *drill.return_session;
        self.lesson_coach = drill.return_coach;
        self.set_drill_view(drill.return_look);
        Some(drill.return_page)
    }

    pub(super) fn drill_return_page(&self) -> Option<Page> {
        self.lesson_drill.as_ref().map(|drill| drill.return_page)
    }

    fn observe_lesson_drill(&mut self, menu: &mut MenuState) -> bool {
        let Some(drill) = &mut self.lesson_drill else {
            return false;
        };
        if self.session.phase == RunPhase::Ready {
            drill.result = Some(DrillResult::Missed(drill.trace.feedback()));
        } else if let Some(end) =
            self.session.splits.get(drill.section).copied().or_else(|| {
                (self.session.phase == RunPhase::Finished).then_some(self.session.elapsed)
            })
        {
            drill.result = Some(DrillResult::Cleared(end - drill.start));
        }
        if drill.result.is_some() {
            menu.go(Page::Finished);
        }
        true
    }
    pub(super) fn starter_guide(&self) -> bool {
        self.starter_guide
    }
    pub(super) fn hide_starter_guide(&mut self) {
        self.starter_guide = false;
    }
    pub fn has_replay(&self) -> bool {
        self.last_replay.is_some()
    }
    pub fn finished_replay(&self) -> Option<&RunReplay> {
        self.last_replay.as_ref()
    }
    pub fn watching_replay(&self) -> bool {
        self.playback.is_some() || self.reference_playback.is_some()
    }
    pub(super) fn watching_lesson_demo(&self) -> bool {
        self.lesson.is_some() && self.playback.is_some()
    }
    pub(super) fn exit_lesson_demo(&mut self) -> Option<Page> {
        if !self.watching_lesson_demo() {
            return None;
        }
        self.playback = None;
        self.replay_paused = false;
        self.previous_eye = self.eye();
        Some(if self.session.phase == RunPhase::Finished {
            Page::Finished
        } else {
            Page::Playing
        })
    }
    pub fn displayed_session(&self) -> &Session {
        if let Some(playback) = &self.reference_playback {
            return &playback.session;
        }
        self.playback
            .as_ref()
            .map_or(&self.session, |playback| &playback.session)
    }
    pub fn replay_progress(&self) -> Option<(usize, usize, bool)> {
        if let Some(playback) = &self.reference_playback {
            return Some((
                playback.cursor,
                playback.track.sample_count() - 1,
                self.replay_paused,
            ));
        }
        self.playback.as_ref().map(|playback| {
            let (cursor, len) = playback.progress();
            (cursor, len, self.replay_paused)
        })
    }
    pub fn displayed_elapsed(&self) -> f64 {
        if let Some(drill) = &self.lesson_drill {
            return match drill.result {
                Some(DrillResult::Ready | DrillResult::Missed(_)) => 0.0,
                Some(DrillResult::Cleared(seconds)) => seconds,
                None => self.session.elapsed - drill.start,
            };
        }
        self.playback
            .as_ref()
            .and_then(Playback::span_elapsed)
            .unwrap_or_else(|| self.displayed_session().elapsed)
    }
    pub fn displayed_input(&self) -> MoveInput {
        if let Some(playback) = &self.reference_playback {
            playback.frame.input
        } else {
            self.playback
                .as_ref()
                .map_or(self.input, Playback::displayed_input)
        }
    }
    pub fn ghost_enabled(&self) -> bool {
        self.ghost.is_some() || self.ghost_preparation.is_some() || self.reference_ghost.is_some()
    }

    pub fn ghost_preparation_progress(&self) -> Option<(usize, usize)> {
        self.ghost_preparation
            .as_ref()
            .map(|pending| pending.preparation.progress())
    }
    pub fn look_angles(&self) -> (f32, f32) {
        if let Some(playback) = &self.reference_playback {
            (playback.frame.pose.yaw, playback.frame.pitch)
        } else if let Some(playback) = &self.playback {
            (playback.yaw, playback.pitch)
        } else {
            self.look.native()
        }
    }

    pub fn third_person_pose(&self) -> Option<(Vec3, f32, bool)> {
        if !self.third_person {
            return None;
        }
        if let Some(playback) = &self.reference_playback {
            let pose = playback.frame.pose;
            return Some((pose.position, pose.yaw, pose.crouched));
        }
        let player = self.displayed_session().player;
        Some((player.position, self.look_angles().0, player.is_crouched()))
    }

    pub fn visual_actor_pose(&self) -> (Vec3, bool) {
        if let Some(playback) = &self.reference_playback {
            let pose = playback.frame.pose;
            (pose.position, pose.crouched)
        } else {
            let player = self.displayed_session().player;
            (player.position, player.is_crouched())
        }
    }
    pub fn ghost_pose(&self) -> Option<(Vec3, f32, bool)> {
        self.reference_ghost
            .as_ref()
            .and_then(|ghost| ghost.sample(self.session.elapsed))
            .or_else(|| {
                self.ghost
                    .as_ref()
                    .and_then(|ghost| ghost.sample(self.session.elapsed))
            })
            .map(|pose| (pose.position, pose.yaw, pose.crouched))
    }

    pub fn set_reference_ghost(&mut self, ghost: ReferenceGhost) -> Result<(), String> {
        if ghost.bonus() != self.selected_bonus || !ghost.matches_course(&self.course) {
            return Err("That record ghost belongs to a different course or revision".into());
        }
        self.clear_ghost();
        self.reference_ghost = Some(ghost);
        Ok(())
    }

    pub fn play_replay(&mut self, replay: RunReplay, ghost: bool) -> Result<(), String> {
        self.play_replay_when(replay, ghost, || Ok(()))
    }

    pub(crate) fn play_replay_when(
        &mut self,
        replay: RunReplay,
        ghost: bool,
        before_play: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        if ghost {
            let bonus = replay.bonus;
            let preparation = NativeGhostPreparation::new(&self.course, replay)
                .map_err(|error| error.to_string())?;
            before_play()?;
            self.ghost_preparation = Some(PendingGhost {
                preparation,
                bonus,
                restart: self.session.phase == RunPhase::Finished || self.watching_replay(),
            });
        } else {
            let playback =
                Playback::new(&self.course, replay).map_err(|error| error.to_string())?;
            before_play()?;
            self.ghost_preparation = None;
            self.session.mark_unranked();
            self.playback = Some(playback);
            self.reference_playback = None;
            self.replay_paused = false;
        }
        Ok(())
    }

    pub fn play_record_span(
        &mut self,
        replay: RunReplay,
        start_tick: f64,
        end_tick: f64,
    ) -> Result<(), String> {
        let mut playback =
            Playback::new(&self.course, replay).map_err(|error| error.to_string())?;
        playback
            .set_span(&self.course, start_tick, end_tick)
            .map_err(|error| error.to_string())?;
        self.ghost_preparation = None;
        self.session.mark_unranked();
        self.playback = Some(playback);
        self.reference_playback = None;
        self.replay_paused = false;
        Ok(())
    }

    pub fn play_reference_replay(&mut self, track: ReferenceGhost) -> Result<(), String> {
        self.play_reference_replay_when(track, || Ok(()))
    }

    pub(crate) fn play_reference_replay_when(
        &mut self,
        track: ReferenceGhost,
        before_play: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let playback = ReferencePlayback::new(&self.course, track)?;
        before_play()?;
        self.ghost_preparation = None;
        self.session.mark_unranked();
        self.playback = None;
        self.reference_playback = Some(playback);
        self.replay_paused = false;
        Ok(())
    }

    pub fn clear_ghost(&mut self) {
        self.ghost_preparation = None;
        self.ghost = None;
        self.reference_ghost = None;
    }

    fn prepare_ghost_step(&mut self) -> Result<bool, ReplayError> {
        let Some(mut pending) = self.ghost_preparation.take() else {
            return Ok(false);
        };
        match pending.preparation.advance(
            &self.course,
            GHOST_PREPARATION_BUDGET,
            GHOST_PREPARATION_COMMANDS,
        ) {
            Ok(Some(ghost)) => {
                if self.selected_bonus != pending.bonus {
                    if !self.set_bonus(pending.bonus) {
                        return Err(ReplayError::Course);
                    }
                } else if pending.restart {
                    self.restart();
                }
                self.clear_ghost();
                self.ghost = Some(ghost);
                Ok(true)
            }
            Ok(None) => {
                self.ghost_preparation = Some(pending);
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    fn new_run(&mut self) {
        if let Some(coach) = &mut self.lesson_coach {
            coach.reset();
        }
        self.ghost_preparation = None;
        self.run_epoch = self.run_epoch.wrapping_add(1);
        self.signed_at_finish = false;
        self.playback = None;
        self.reference_playback = None;
        self.replay_paused = false;
        self.jump_pending = false;
        self.stage_restart_pending = false;
    }

    fn reset_view(&mut self) {
        self.look = SourceLook::new(
            self.session.spawn_source_yaw(&self.course),
            self.session.spawn_pitch(&self.course),
        );
        self.previous_eye = self.eye();
    }

    fn replace(&mut self, loaded: LoadedCourse, settings: &Settings) {
        *self = self.replacement(loaded, settings);
    }

    fn replacement(&self, loaded: LoadedCourse, settings: &Settings) -> Self {
        let mut next = Self::from_loaded(loaded);
        next.third_person = self.third_person;
        next.course_version = self.course_version + 1;
        next.run_epoch = self.run_epoch + 1;
        next.set_run_options(run_options(settings));
        next
    }

    pub(crate) fn play_loaded_replay(
        &mut self,
        loaded: LoadedCourse,
        settings: &Settings,
        replay: RunReplay,
        ghost: bool,
        before_play: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let mut next = self.replacement(loaded, settings);
        next.play_replay_when(replay, ghost, before_play)?;
        *self = next;
        Ok(())
    }
}

pub(crate) fn run_options(settings: &Settings) -> RunOptions {
    RunOptions {
        auto_bhop: settings.auto_bhop,
        unrestricted_start: settings.unrestricted_start,
    }
}

#[derive(Component)]
pub struct ViewCamera;

pub fn run(
    course: Course,
    replay: Option<StartupReplay>,
    benchmark: Option<BenchRequest>,
) -> Result<(), String> {
    let library = MapLibrary::read();
    let mut menu = MenuState::new(&library.directory);
    let mut game = Game::new(course);
    game.set_run_options(run_options(&menu.settings));
    if let Some(replay) = replay {
        game.start_replay(replay)?;
        menu.go(Page::Playing);
    }
    let bench_mode = benchmark.is_some();
    if bench_mode {
        menu.settings.vsync = false;
        menu.settings.menu_motion = false;
        menu.settings.environmental_audio = false;
    }
    let mut online = Online::default();
    if !bench_mode && let Ok(address) = std::env::var("SURF_CONNECT") {
        online.queue(crate::online::Action::JoinAddress(address));
    }
    let plugins = DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "Surf / Bevy".to_owned(),
            resolution: (1280, 800).into(),
            visible: !bench_mode,
            ..default()
        }),
        ..default()
    });
    #[cfg(windows)]
    let plugins = render_compat::configure(plugins);
    let mut app = App::new();
    app.insert_resource(game)
        .insert_resource(menu)
        .insert_resource(library)
        .insert_resource(online)
        .insert_resource(if bench_mode {
            // Keep measuring when Windows withholds redraws from an occluded window.
            let updates = UpdateMode::reactive(std::time::Duration::ZERO);
            WinitSettings {
                focused_mode: updates,
                unfocused_mode: updates,
            }
        } else {
            WinitSettings {
                focused_mode: UpdateMode::Continuous,
                unfocused_mode: UpdateMode::reactive_low_power(std::time::Duration::from_millis(
                    250,
                )),
            }
        })
        .add_message::<menu::MenuRequest>()
        .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
        .insert_resource(ClearColor(BACKGROUND))
        .insert_resource(GlobalAmbientLight {
            brightness: 0.0,
            ..default()
        })
        .add_plugins(plugins)
        .add_plugins(crate::ui_font::UiFontPlugin)
        .add_plugins(world::SurfdMaterialPlugin)
        .add_systems(Startup, (spawn_camera, configure_map_assets).chain())
        .add_systems(FixedUpdate, advance)
        .add_systems(
            Update,
            (
                menu::apply_actions.run_if(not(resource_exists::<FrameBenchmark>)),
                online::update,
                poll_load,
                prepare_ghost,
                world::update_world,
                update_camera,
                world::update_map_view,
                crate::online_world::update_players,
                menu::render,
                menu::update_hud,
            )
                .chain(),
        );
    if let Some(request) = benchmark {
        let benchmark = FrameBenchmark::new(request);
        crate::bench_render_trace::install(&mut app);
        if benchmark.render_diagnostics_enabled()
            && !app.is_plugin_added::<bevy::render::diagnostic::RenderDiagnosticsPlugin>()
        {
            app.add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin);
        }
        if benchmark.crowd_enabled() {
            app.add_systems(
                Update,
                bench::place_crowd
                    .after(update_camera)
                    .before(crate::online_world::update_players),
            );
        }
        app.insert_resource(benchmark);
        app.add_systems(Update, bench::sample.after(menu::update_hud));
    } else {
        app.add_systems(
            PreUpdate,
            (online::keyboard, menu::keyboard, read_controls)
                .chain()
                .after(InputSystems),
        );
    }
    #[cfg(feature = "client-audio")]
    app.add_plugins(crate::ambient::AmbientPlugin);
    app.add_plugins(crate::flythrough::FlythroughPlugin);
    match app.run() {
        AppExit::Success => Ok(()),
        AppExit::Error(_) => Err("Desktop client exited with an error".into()),
    }
}

fn configure_map_assets(
    mut library: ResMut<MapLibrary>,
    device: Option<Res<RenderDevice>>,
    mut benchmark: Option<ResMut<FrameBenchmark>>,
    mut game: ResMut<Game>,
    mut menu: ResMut<MenuState>,
    mut exit: MessageWriter<AppExit>,
) {
    let use_bc = device.is_some_and(|device| {
        device
            .features()
            .contains(WgpuFeatures::TEXTURE_COMPRESSION_BC)
    }) && (benchmark.is_none() || std::env::var_os("SURF_BENCH_DECODED").is_none());
    library.set_compressed_textures(use_bc);
    if let Some(bench) = benchmark.as_deref_mut()
        && let Some(request) = bench.request.take()
    {
        bench.started_load();
        let parallel_packs =
            !std::env::var("SURF_BENCH_SERIAL_PACKS").is_ok_and(|value| value == "1");
        match library.start_benchmark_load(request.source, parallel_packs) {
            Ok(job) => {
                game.pending = Some(job);
                bench.replay = Some(request.replay);
                menu.go(Page::Maps);
                menu.set_status("Loading benchmark course…".into());
            }
            Err(error) => {
                eprintln!("{error}");
                exit.write(AppExit::error());
            }
        }
    }
}

fn spawn_camera(
    mut commands: Commands,
    game: Res<Game>,
    menu: Res<MenuState>,
    mut images: ResMut<Assets<Image>>,
    benchmark: Option<ResMut<FrameBenchmark>>,
) {
    let (yaw, pitch) = game.look_angles();
    let mut camera = commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: menu.settings.vertical_fov_radians(),
            near: 1.0,
            far: 65_536.0,
            ..default()
        }),
        Tonemapping::None,
        DebandDither::Disabled,
        DistanceFog::default(),
        Transform::from_translation(game.eye()).with_rotation(rotation(yaw, pitch)),
        ViewCamera,
    ));
    if let Some(mut benchmark) = benchmark {
        let mut image = Image::new_target_texture(
            bench::SIZE.x,
            bench::SIZE.y,
            TextureFormat::Rgba8UnormSrgb,
            None,
        );
        image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
        let target = images.add(image);
        camera.insert((
            bevy::camera::RenderTarget::from(target.clone()),
            IsDefaultUiCamera,
        ));
        benchmark.target = Some(target);
    }
}

fn rotation(yaw: f32, pitch: f32) -> Quat {
    Quat::from_rotation_y(yaw) * Quat::from_rotation_x(pitch)
}

fn read_controls(
    mut game: ResMut<Game>,
    mut menu: ResMut<MenuState>,
    online: Res<Online>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut mouse: MessageReader<MouseMotion>,
    mut moved: MessageReader<CursorMoved>,
    scroll: Res<AccumulatedMouseScroll>,
    window: Single<(Entity, &mut Window, &mut CursorOptions), With<PrimaryWindow>>,
    mut was_playing: Local<bool>,
    mut cursor_motion: Local<CursorMotion>,
    mut raw_motion: Local<Vec<Vec2>>,
) {
    let (window_id, mut window, mut cursor) = window.into_inner();
    if !window.focused && menu.page == Page::Playing {
        menu.go(Page::Pause);
    }
    let playing = menu.page == Page::Playing;
    if *was_playing
        && !playing
        && game.session.phase == RunPhase::Running
        && !game.watching_replay()
    {
        game.session.mark_unranked();
    }
    *was_playing = playing;
    let live_input = playing && !online.is_typing() && online.spectating.is_none();
    let aim = live_input && !game.watching_replay();
    let grab = match (aim, cfg!(windows)) {
        (true, true) => CursorGrabMode::Confined,
        (true, false) => CursorGrabMode::Locked,
        (false, _) => CursorGrabMode::None,
    };
    let changed_grab = cursor.grab_mode != grab;
    if changed_grab {
        cursor.grab_mode = grab;
        *cursor_motion = CursorMotion::default();
    }
    if cursor.visible == aim {
        cursor.visible = !aim;
    }
    raw_motion.clear();
    raw_motion.extend(
        mouse
            .read()
            .map(|event| event.delta)
            .filter(|delta| *delta != Vec2::ZERO),
    );
    let cursor_delta = moved
        .read()
        .filter(|event| event.window == window_id)
        .fold(Vec2::ZERO, |delta, event| {
            delta + cursor_motion.observe(event.position)
        });
    let window_size = Vec2::new(window.width(), window.height());
    if grab == CursorGrabMode::Confined
        && (changed_grab
            || (raw_motion.is_empty()
                && cursor_delta != Vec2::ZERO
                && cursor_motion.near_edge(window_size)))
    {
        let center = window_size * 0.5;
        cursor_motion.recenter(center);
        window.set_cursor_position(Some(center));
    }
    if !live_input {
        game.input = MoveInput::default();
        game.turn_rate = 0.0;
        game.jump_pending = false;
        game.stage_restart_pending = false;
        return;
    }
    if menu
        .settings
        .just_pressed(ControlAction::ToggleCamera, &keys, &buttons, scroll.delta)
    {
        game.third_person = !game.third_person;
    }
    if game.watching_replay() {
        if keys.just_pressed(KeyCode::Backspace) {
            if let Some(page) = game.exit_lesson_demo() {
                menu.go(page);
                menu.refresh();
                return;
            }
            game.ghost_preparation = None;
            game.playback = None;
            game.reference_playback = None;
            game.replay_paused = false;
            game.reset_view();
            menu.go(if game.session.phase == RunPhase::Finished {
                Page::Finished
            } else {
                Page::Pause
            });
        } else if keys.just_pressed(KeyCode::Space) {
            game.replay_paused = !game.replay_paused;
        } else if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyR) {
            let game = &mut *game;
            let restart = keys.just_pressed(KeyCode::KeyR);
            if let Some(playback) = &mut game.reference_playback {
                playback.seek(if restart { 0 } else { playback.cursor + 1 });
                playback.accumulator = 0.0;
            } else if let Some(playback) = &mut game.playback {
                if restart {
                    playback.restart(&game.course);
                } else {
                    playback.tick(&game.course);
                }
            }
            game.replay_paused = !restart;
        }
        return;
    }
    let restart = menu
        .settings
        .just_pressed(ControlAction::Restart, &keys, &buttons, scroll.delta);
    if restart {
        game.restart();
    }
    game.stage_restart_pending |=
        menu.settings
            .just_pressed(ControlAction::RestartStage, &keys, &buttons, scroll.delta);
    if menu
        .settings
        .just_pressed(ControlAction::CycleYaw, &keys, &buttons, scroll.delta)
    {
        menu.settings.cycle_yaw_rate();
    }
    if menu
        .settings
        .just_pressed(ControlAction::SavePosition, &keys, &buttons, scroll.delta)
    {
        game.save_position();
    }
    if menu
        .settings
        .just_pressed(ControlAction::LoadPosition, &keys, &buttons, scroll.delta)
    {
        game.restore_position();
    }
    if menu
        .settings
        .just_pressed(ControlAction::TogglePractice, &keys, &buttons, scroll.delta)
    {
        if game.practice_name.is_some() {
            game.set_practice(None);
        } else {
            game.save_position();
            game.restore_position();
        }
    }
    if !restart && !changed_grab {
        game.look.apply_input(
            &raw_motion,
            cursor_delta,
            window.scale_factor(),
            menu.settings.mouse_degrees_per_count(),
            menu.settings.invert_mouse,
        );
    }
    let active = |action| menu.settings.active(action, &keys, &buttons, scroll.delta);
    let axis = |positive, negative| {
        f32::from(u8::from(active(positive))) - f32::from(u8::from(active(negative)))
    };
    game.turn_rate = f64::from(axis(ControlAction::TurnLeft, ControlAction::TurnRight))
        * menu.settings.yaw_rate(active(ControlAction::AlternateYaw));
    game.jump_pending |=
        menu.settings
            .just_pressed(ControlAction::Jump, &keys, &buttons, scroll.delta);
    game.input = MoveInput {
        forward: axis(ControlAction::Forward, ControlAction::Back),
        strafe: axis(ControlAction::Right, ControlAction::Left),
        yaw: game.look.native().0,
        jump: active(ControlAction::Jump),
        crouch: active(ControlAction::Crouch),
    };
}

pub(super) fn advance(mut game: ResMut<Game>, mut menu: ResMut<MenuState>, online: Res<Online>) {
    if menu.page != Page::Playing || online.spectating.is_some() || game.pending.is_some() {
        return;
    }
    let game = &mut *game;
    if game.drill_result().is_some() {
        return;
    }
    if let Some(playback) = &mut game.reference_playback {
        if !game.replay_paused && !playback.advance() {
            game.replay_paused = true;
        }
        return;
    }
    if let Some(playback) = &mut game.playback {
        if !game.replay_paused && !playback.tick(&game.course) {
            game.replay_paused = true;
        }
        return;
    }
    let position = game.session.player.position;
    let phase = game.session.phase;
    game.previous_eye = game.eye();
    game.look.turn(game.turn_rate * f64::from(TICK_SECONDS));
    let (yaw, pitch) = game.look.native();
    let mut input = game.input;
    input.yaw = yaw;
    input.jump |= std::mem::take(&mut game.jump_pending);
    let command = TickCommand {
        input,
        pitch,
        restart_stage: std::mem::take(&mut game.stage_restart_pending)
            && game.lesson_drill.is_none(),
        source_yaw_degrees: Some(game.look.yaw),
    };
    if let Some(drill) = &mut game.lesson_drill {
        drill.trace.observe(&game.session.player, command);
    }
    if let Some(coach) = &mut game.lesson_coach {
        if phase == RunPhase::Running && coach.supports_section(game.session.splits.len()) {
            coach.observe(&game.session.player, command);
        } else {
            coach.reset();
        }
    }
    let yaw_override = game.session.tick_command(&game.course, command);
    if phase == RunPhase::Running
        && game.session.phase == RunPhase::Ready
        && game.tutorial_lesson().is_some()
        && game.lesson_drill.is_none()
    {
        game.lesson_retries = game.lesson_retries.saturating_add(1);
    }
    if let Some(yaw) = yaw_override {
        game.look.yaw = game
            .session
            .source_yaw_override()
            .unwrap_or_else(|| native_yaw_to_source(yaw));
    }
    if let Some(pitch) = game.session.pitch_override() {
        game.look.set_native_pitch(pitch);
    }
    if yaw_override.is_some()
        || game.session.player.position.distance_squared(position) > 128.0 * 128.0
    {
        game.previous_eye = game.eye();
    }
    if game.observe_lesson_drill(&mut menu) {
        return;
    }
    if game.session.phase == RunPhase::Finished && game.replay_epoch != Some(game.run_epoch) {
        game.replay_epoch = Some(game.run_epoch);
        game.last_replay = if game.tutorial_lesson().is_some() {
            None
        } else {
            game.session.take_finished_replay(&game.course)
        };
        game.signed_at_finish = online.is_signed_in();
        menu.go(Page::Finished);
    }
}

fn prepare_ghost(mut game: ResMut<Game>, mut menu: ResMut<MenuState>) {
    if game.ghost_preparation.is_none() {
        return;
    }
    if game.pending.is_some() {
        game.ghost_preparation = None;
        return;
    }
    if let Err(error) = game.prepare_ghost_step() {
        menu.set_status(format!("Could not load ghost ({error})"));
    }
}

fn update_camera(
    mut game: ResMut<Game>,
    menu: Res<MenuState>,
    online: Res<Online>,
    time: Res<Time<Fixed>>,
    camera: Single<(&mut Transform, &mut Projection), With<ViewCamera>>,
) {
    let (mut camera, mut projection) = camera.into_inner();
    let (eye, yaw, pitch) =
        if let Some(state) = online.viewed_pose().filter(|_| online.show_players(&game)) {
            (
                Vec3::from_array(state.position) + Vec3::Y * state.camera_eye_height(),
                state.yaw,
                state.pitch,
            )
        } else if let Some(playback) = &game.reference_playback {
            let frame = playback.frame;
            (
                frame.pose.position + Vec3::Y * frame.eye_height,
                frame.pose.yaw,
                frame.pitch,
            )
        } else if let Some(playback) = &game.playback {
            (
                playback.session.player.position + Vec3::Y * playback.session.player.eye_height(),
                playback.yaw,
                playback.pitch,
            )
        } else {
            let eye = if menu.page == Page::Playing && menu.settings.smooth_camera {
                game.previous_eye.lerp(game.eye(), time.overstep_fraction())
            } else {
                game.eye()
            };
            let (yaw, pitch) = game.look.native();
            (eye, yaw, pitch)
        };
    let orientation = rotation(yaw, pitch);
    let position = if game.third_person && online.spectating.is_none() {
        let game = &mut *game;
        let target = eye + orientation * Vec3::Z * 112.0 + Vec3::Y * 14.0;
        game.session.clip_camera(&game.course, eye, target)
    } else {
        eye
    };
    if camera.translation != position || camera.rotation != orientation {
        camera.translation = position;
        camera.rotation = orientation;
    }
    let fov = menu.settings.vertical_fov_radians();
    if matches!(&*projection, Projection::Perspective(current) if current.fov != fov)
        && let Projection::Perspective(current) = &mut *projection
    {
        current.fov = fov;
    }
}

pub(super) fn poll_load(
    mut game: ResMut<Game>,
    mut menu: ResMut<MenuState>,
    mut benchmark: Option<ResMut<FrameBenchmark>>,
    mut exit: MessageWriter<AppExit>,
) {
    while let Some(event) = game.pending.as_ref().and_then(LoadJob::poll) {
        match event {
            LoadEvent::Progress(status) => menu.set_status(status),
            LoadEvent::Finished(result) => {
                game.pending = None;
                match result {
                    Ok(course) => {
                        if let Some(bench) = benchmark.as_deref_mut() {
                            bench.record_map(&course.course);
                        }
                        game.replace(*course, &menu.settings);
                        if let Some(bench) = benchmark.as_deref_mut()
                            && let Some(replay) = bench.replay.take()
                        {
                            let result = game.start_replay(replay).and_then(|()| {
                                game.seek_benchmark_replay(
                                    bench.start_tick.unwrap_or(0),
                                    bench.hold_tick,
                                )
                            });
                            if let Err(error) = result {
                                eprintln!("{error}");
                                exit.write(AppExit::error());
                                return;
                            }
                        }
                        menu.set_status(format!("{} is ready", game.course.name));
                        menu.go(Page::Playing);
                    }
                    Err(error) => {
                        if benchmark.is_some() {
                            eprintln!("{error}");
                            exit.write(AppExit::error());
                            return;
                        }
                        menu.set_status(error);
                        menu.refresh();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectator_camera_uses_transmitted_eye_height_with_legacy_stance_fallback() {
        use bevy_surf::network::{PlayerInfo, PlayerState, RoomInfo};
        use std::time::{Duration, Instant};

        let course = Course::practice();
        let legacy_pose: PlayerState = serde_json::from_str(
            r#"{"position":[12.0,32.0,50.0],"yaw":0.5,"pitch":-0.2,"elapsed":7.25,"finished":false,"crouched":true}"#,
        )
        .unwrap();
        let mut online = Online::default();
        online.room = Some(RoomInfo {
            name: "Test room".into(),
            map: course.name.clone(),
            players: 2,
            capacity: 4,
            room_id: None,
            map_remaining_ms: None,
        });
        online.spectating = Some(1);
        online.players.push(PlayerInfo {
            id: 1,
            name: "Surfer".into(),
            state: Some(PlayerState {
                eye_height: Some(55.5),
                ..legacy_pose
            }),
            spectating: None,
            identity: None,
        });

        let mut app = App::new();
        app.insert_resource(Game::new(course))
            .insert_resource(MenuState::new(std::path::Path::new("target")))
            .insert_resource(online)
            .init_resource::<Time<Fixed>>()
            .add_systems(Update, update_camera);
        let camera = app
            .world_mut()
            .spawn((ViewCamera, Transform::default(), Projection::default()))
            .id();
        app.update();
        let view = app.world().get::<Transform>(camera).unwrap();
        assert_eq!(view.translation, Vec3::new(12.0, 87.5, 50.0));
        assert_eq!(view.rotation, rotation(0.5, -0.2));

        app.world_mut().resource_mut::<Online>().players[0].state = Some(legacy_pose);
        app.update();
        assert_eq!(
            app.world().get::<Transform>(camera).unwrap().translation,
            Vec3::new(12.0, 79.0, 50.0)
        );

        let start = Instant::now();
        {
            let mut online = app.world_mut().resource_mut::<Online>();
            online.id = Some(2);
            let watcher = PlayerInfo {
                id: 2,
                name: "Watcher".into(),
                state: None,
                spectating: Some(1),
                identity: None,
            };
            let mut surfer = online.players[0].clone();
            surfer.state = Some(PlayerState {
                eye_height: Some(64.0),
                motion: Some([0, 1]),
                ..legacy_pose
            });
            online.accept_roster(vec![surfer.clone(), watcher.clone()], start);
            surfer.state = Some(PlayerState {
                position: [62.0, 32.0, 50.0],
                motion: Some([50, 1]),
                ..legacy_pose
            });
            online.accept_roster(vec![surfer, watcher], start + Duration::from_millis(50));
            online.sample_room_poses(start + Duration::from_millis(65));
        }
        app.update();
        let shown = app.world().get::<Transform>(camera).unwrap().translation;
        assert!(shown.distance(Vec3::new(37.0, 87.5, 50.0)) < 1e-4);
    }

    fn ghost_fixture() -> (Game, RunReplay) {
        use bevy_surf::course::{BonusRoute, Brush, PracticeSpawn, Zone, ZoneKind};

        let mut course = Course::practice();
        course.spawn = Vec3::Y * 0.05;
        course.spawn_yaw = 0.0;
        course.brushes = vec![Brush::box_at(
            Vec3::new(-100.0, -50.0, -300.0),
            Vec3::new(100.0, 0.0, 100.0),
        )];
        course.zones = [(0.0, ZoneKind::Start), (-140.0, ZoneKind::Finish)]
            .into_iter()
            .map(|(z, kind)| Zone {
                brush: Brush::box_at(
                    Vec3::new(-80.0, -8.0, z - 8.0),
                    Vec3::new(80.0, 120.0, z + 8.0),
                ),
                kind,
            })
            .collect();
        course.bonus_routes.push(BonusRoute {
            number: 1,
            name: "Bonus 1".into(),
            spawn: PracticeSpawn {
                name: "Bonus 1".into(),
                position: course.spawn,
                yaw: course.spawn_yaw,
                pitch: 0.0,
                source_yaw_degrees: None,
            },
            zones: course
                .zones
                .iter()
                .map(|zone| Zone {
                    brush: zone.brush.clone(),
                    kind: zone.kind.clone(),
                })
                .collect(),
            revision: "bonus-fixture-v1".into(),
        });
        let game = Game::new(course);
        let mut recorded = Session::new(&game.course);
        let input = MoveInput {
            forward: 1.0,
            ..Default::default()
        };
        for _ in 0..200 {
            recorded.tick(&game.course, input);
            if recorded.phase == RunPhase::Finished {
                break;
            }
        }
        let replay = recorded.take_finished_replay(&game.course).unwrap();
        (game, replay)
    }

    fn finish_ghost_preparation(game: &mut Game) -> Result<(), ReplayError> {
        for _ in 0..200 {
            if game.prepare_ghost_step()? {
                return Ok(());
            }
        }
        panic!("short ghost fixture should finish preparing");
    }

    #[test]
    fn air_strafe_drill_measures_an_unpaired_right_strafe_and_resets_on_retry() {
        let id = "tutorial-strafe";
        let course = tutorial::course(id).unwrap();
        let session = Session::new(&course);
        let demo = tutorial::demonstration(&course).unwrap();
        let mut game = Game::from_loaded(LoadedCourse {
            course,
            session,
            lesson: tutorial::lesson(id),
            starter_guide: false,
        });
        game.practice_lesson_section(0, Page::Pause).unwrap();
        let after_entry = game.lesson_drill.as_ref().unwrap().entry.len();
        game.restart();
        let mut menu = MenuState::new(&std::env::temp_dir());
        menu.go(Page::Playing);
        let mut app = App::new();
        app.insert_resource(game)
            .insert_resource(menu)
            .init_resource::<Online>()
            .add_systems(FixedUpdate, advance);

        let right = MoveInput {
            strafe: 1.0,
            ..Default::default()
        };
        let inputs = demo.commands[after_entry..]
            .iter()
            .map(|command| command.input)
            .chain(std::iter::repeat_n(right, 150));
        for input in inputs {
            app.world_mut().resource_mut::<Game>().input = input;
            app.world_mut().run_schedule(FixedUpdate);
            if app.world().resource::<Game>().drill_result().is_some() {
                break;
            }
        }
        let Some(DrillResult::Missed(feedback)) = app.world().resource::<Game>().drill_result()
        else {
            panic!("the right strafe without the demo's turn should miss the offset deck");
        };
        let Some(DrillFinding::AirTurn {
            right_degrees,
            paired_degrees,
            strafe_ms,
        }) = feedback.finding
        else {
            panic!("the first airborne strafe should produce an input finding");
        };
        assert_eq!((right_degrees, paired_degrees), (0.0, 0.0));
        assert!(strafe_ms >= 75);
        assert!(feedback.peak_speed >= 260.0);

        app.world_mut().resource_mut::<Game>().restart();
        app.world_mut()
            .resource_mut::<MenuState>()
            .go(Page::Playing);
        for command in &demo.commands[after_entry..] {
            let mut game = app.world_mut().resource_mut::<Game>();
            game.look = SourceLook::new(command.source_yaw_degrees.unwrap(), command.pitch);
            game.input = command.input;
            app.world_mut().run_schedule(FixedUpdate);
        }
        let game = app.world().resource::<Game>();
        assert!(matches!(game.drill_result(), Some(DrillResult::Cleared(_))));
        assert!(
            game.lesson_drill
                .as_ref()
                .unwrap()
                .trace
                .feedback()
                .finding
                .is_none()
        );
    }

    #[test]
    fn enabling_a_native_ghost_during_a_live_run_keeps_the_attempt() {
        let (mut game, replay) = ghost_fixture();
        let input = MoveInput {
            forward: 1.0,
            ..Default::default()
        };
        for _ in 0..25 {
            game.session.tick(&game.course, input);
        }
        assert_eq!(game.session.phase, RunPhase::Running);
        let elapsed = game.session.elapsed;
        let epoch = game.run_epoch;
        let pose = game.session.player.position;

        game.play_replay(replay, true).unwrap();
        assert_eq!(game.run_epoch, epoch);
        assert_eq!(game.session.elapsed, elapsed);
        assert_eq!(game.session.player.position, pose);
        assert!(game.ghost_enabled());
        assert!(game.ghost_preparation_progress().is_some());
        finish_ghost_preparation(&mut game).unwrap();
        assert_eq!(game.run_epoch, epoch);
        assert_eq!(game.session.elapsed, elapsed);
        assert!(game.ghost_preparation_progress().is_none());
        assert!(game.ghost_pose().unwrap().0.distance(pose) < 1e-4);

        game.restart();
        assert_eq!(game.session.phase, RunPhase::Ready);
        assert!((game.ghost_pose().unwrap().0.z + 24.0).abs() < 1e-4);
    }

    #[test]
    fn canceling_or_replacing_a_course_discards_pending_ghost_preparation() {
        let (mut game, replay) = ghost_fixture();
        game.play_replay(replay.clone(), true).unwrap();
        assert!(game.ghost_enabled());
        game.clear_ghost();
        assert!(!game.prepare_ghost_step().unwrap());
        assert!(!game.ghost_enabled());

        game.play_replay(replay, true).unwrap();
        let course = Course::practice();
        let session = Session::new(&course);
        game.replace(
            LoadedCourse {
                course,
                session,
                lesson: None,
                starter_guide: false,
            },
            &Settings::default(),
        );
        assert!(game.ghost_preparation_progress().is_none());
        assert!(!game.prepare_ghost_step().unwrap());
        assert!(!game.ghost_enabled());
    }

    #[test]
    fn failed_pending_bonus_ghost_retains_the_live_run_and_existing_ghost() {
        let (mut game, replay) = ghost_fixture();
        game.play_replay(replay, true).unwrap();
        finish_ghost_preparation(&mut game).unwrap();
        let input = MoveInput {
            forward: 1.0,
            ..Default::default()
        };
        for _ in 0..25 {
            game.session.tick(&game.course, input);
        }
        let epoch = game.run_epoch;
        let elapsed = game.session.elapsed;
        let pose = game.session.player.position;
        let ghost_pose = game.ghost_pose();

        let mut recorded = Session::new(&game.course);
        recorded.select_bonus(&game.course, Some(1)).unwrap();
        for _ in 0..200 {
            recorded.tick(&game.course, input);
            if recorded.phase == RunPhase::Finished {
                break;
            }
        }
        let bonus = recorded.take_finished_replay(&game.course).unwrap();
        let mut invalid = bonus.clone();
        invalid.commands[0].input.forward = 2.0;
        let mut incomplete = bonus;
        incomplete.commands.truncate(1);
        for replay in [invalid, incomplete] {
            game.play_replay(replay, true).unwrap();
            assert_eq!(game.selected_bonus(), None);
            assert_eq!(game.run_epoch, epoch);
            assert_eq!(game.session.player.position, pose);
            assert_eq!(game.ghost_pose(), ghost_pose);
            assert!(finish_ghost_preparation(&mut game).is_err());
            assert_eq!(game.selected_bonus(), None);
            assert_eq!(game.run_epoch, epoch);
            assert_eq!(game.session.elapsed, elapsed);
            assert_eq!(game.session.player.position, pose);
            assert_eq!(game.ghost_pose(), ghost_pose);
        }
    }

    #[test]
    fn unchanged_run_options_preserve_live_look_and_attempt() {
        let mut game = Game::new(Course::practice());
        game.look.turn(30.0);
        let yaw = game.look.yaw;
        let epoch = game.run_epoch;

        game.set_run_options(RunOptions::default());
        assert_eq!(game.look.yaw, yaw);
        assert_eq!(game.run_epoch, epoch);

        game.set_run_options(RunOptions {
            auto_bhop: false,
            ..RunOptions::default()
        });
        assert_ne!(game.look.yaw, yaw);
        assert_ne!(game.run_epoch, epoch);
        assert!(!game.session.options().auto_bhop);
    }
}
