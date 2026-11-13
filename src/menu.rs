use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use bevy::asset::RenderAssetUsages;
use bevy::clipboard::{Clipboard, ClipboardRead};
use bevy::ecs::hierarchy::ChildSpawnerCommands;
use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::picking::{
    Pickable,
    events::{PointerClick, PointerOut, PointerOver},
    pointer::PointerButton,
};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::ui::{BackgroundGradient, ColorStop, Gradient, LinearGradient, RelativeCursorPosition};
use bevy::window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode};
use bevy_surf::catalog::MapMode;
use bevy_surf::session::{RunOptions, RunPhase};
use serde::{Deserialize, Serialize};

use crate::cosmetics::{AvatarSkin, KnifeFinish};
use crate::flythrough::{BackdropCaption, BackdropImage};
use crate::library::{MapEntry, MapLibrary};
use crate::online::{Online, OnlinePage};
use crate::online_ui;
use crate::view::Game;

#[path = "menu_cosmetics.rs"]
mod menu_cosmetics;
#[path = "menu_hud.rs"]
mod menu_hud;
#[path = "menu_pause_board.rs"]
mod menu_pause_board;
#[path = "menu_settings.rs"]
mod menu_settings;
#[path = "menu_training.rs"]
mod menu_training;
#[path = "preferences.rs"]
mod preferences;
pub use menu_hud::update_hud;
use menu_hud::{crosshair, spawn_hud};
use menu_settings::settings;
use menu_training::TrainingProgress;
use preferences::{Binding, CrosshairPart, CrosshairStyle, MAX_BINDINGS, valid_yaw_rate};
pub use preferences::{ControlAction, Settings};

const INK: Color = Color::srgb_u8(237, 243, 245);
const MUTED: Color = Color::srgb_u8(191, 205, 213);
const MINT: Color = Color::srgb_u8(165, 216, 219);
const PANEL: Color = Color::srgba_u8(42, 57, 66, 250);
const EDGE: Color = Color::srgb_u8(63, 86, 99);
const TILE: Color = Color::srgb_u8(58, 76, 87);
const INSET: Color = Color::srgb_u8(19, 28, 34);
const ACTIVE: Color = Color::srgb_u8(52, 89, 106);
const GOLD: Color = Color::srgb_u8(228, 199, 149);
const MAPS_PER_PAGE: usize = 6;
const SPAWNS_PER_PAGE: usize = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Maps,
    Practice,
    Training,
    Servers,
    Players,
    Records,
    Profile,
    Friends,
    Store,
    Moderation,
    Settings,
    Pause,
    Playing,
    Finished,
}

#[derive(Message, Clone, Copy)]
pub enum MenuRequest {
    QuickPlay,
    Equip(crate::online::Equip),
    LeaveServer,
    EnterLocalMap,
    StartLesson(&'static str),
    WatchLesson(&'static str),
    StartPathCourse(&'static str),
    WatchReplay,
    WatchReferenceReplay,
    ExportReplay,
    ToggleGhost,
    PauseRecordGhost,
    PauseWatchRecordRoute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsTab {
    Controls,
    Turns,
    Mouse,
    Gameplay,
    Video,
    Audio,
    Hud,
    Crosshair,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsField {
    Search,
    Sensitivity,
    Fov,
    Volume,
    AddYaw,
    AlternateYaw,
    CrosshairColor,
    CrosshairCode,
}

#[derive(Clone, Copy)]
enum Toggle {
    Keys,
    Crosshair,
    Vsync,
    Fullscreen,
    MenuMotion,
    EnvironmentalAudio,
    InvertMouse,
    SmoothCamera,
    AutoBhop,
    UnrestrictedStart,
    Turns,
    CenterSpeed,
    StrafeSync,
    CompareSplits,
    CompareSpeed,
    Colorblind,
    Diagnostics,
    CrosshairT,
}

#[derive(Resource)]
pub struct MenuState {
    pub page: Page,
    pub settings: Settings,
    settings_path: PathBuf,
    home_page: Page,
    dirty: bool,
    notice: String,
    notice_until: std::time::Instant,
    query: String,
    map_page: usize,
    map_mode: MapMode,
    local_maps_only: bool,
    spawn_page: usize,
    record_page: usize,
    records: BTreeMap<String, BestRun>,
    training: TrainingProgress,
    show_online_records: bool,
    settings_tab: SettingsTab,
    settings_search: String,
    settings_scroll_y: f32,
    content_scroll_y: f32,
    edit: Option<(SettingsField, String)>,
    capture: Option<(ControlAction, Option<usize>)>,
    finished_epoch: Option<u64>,
    finish_pb: Option<f64>,
    finish_xp: usize,
    lesson_result: Option<menu_training::LessonResult>,
    actions: Vec<Action>,
}

#[derive(Deserialize, Serialize)]
struct BestRun {
    seconds: f64,
    splits: Vec<f64>,
    #[serde(default)]
    speeds: Vec<f32>,
}

impl MenuState {
    pub(crate) fn open_verified_records(&mut self) {
        self.show_online_records = true;
        self.go(Page::Records);
    }

    pub fn new(data: &Path) -> Self {
        let settings_path = data.join("settings.json");
        let records = fs::read(data.join("records.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let settings = fs::read(&settings_path)
            .map_or_else(|_| Settings::default(), |bytes| Settings::read(&bytes));
        Self {
            page: Page::Home,
            settings,
            settings_path,
            home_page: Page::Home,
            dirty: true,
            notice: String::new(),
            notice_until: std::time::Instant::now(),
            query: String::new(),
            map_page: 0,
            map_mode: MapMode::Surf,
            local_maps_only: false,
            spawn_page: 0,
            record_page: 0,
            records,
            training: TrainingProgress::read(data),
            show_online_records: true,
            settings_tab: SettingsTab::Controls,
            settings_search: String::new(),
            settings_scroll_y: 0.0,
            content_scroll_y: 0.0,
            edit: None,
            capture: None,
            finished_epoch: None,
            finish_pb: None,
            finish_xp: 0,
            lesson_result: None,
            actions: Vec::new(),
        }
    }

    pub fn go(&mut self, page: Page) {
        if self.page != page {
            if matches!(page, Page::Training | Page::Finished) {
                self.notice.clear();
            }
            self.page = page;
            self.dirty = true;
            self.edit = None;
            self.capture = None;
            self.settings_scroll_y = 0.0;
            self.reset_content_scroll();
        }
        if matches!(page, Page::Home | Page::Pause) {
            self.home_page = page;
        }
    }

    pub(super) fn reset_content_scroll(&mut self) {
        self.content_scroll_y = 0.0;
    }

    fn browse_maps(&mut self, local_only: bool) {
        if self.local_maps_only != local_only {
            self.local_maps_only = local_only;
            self.query.clear();
            self.map_page = 0;
            self.dirty = true;
        }
        self.go(Page::Maps);
    }

    pub fn set_status(&mut self, message: String) {
        self.notice = message;
        self.notice_until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    }

    pub fn refresh(&mut self) {
        self.dirty = true;
    }

    pub(crate) fn local_loadout(&self) -> crate::cosmetics::Loadout {
        crate::cosmetics::Loadout {
            avatar_skin: self.settings.avatar_skin,
            knife_finish: self.settings.knife_finish,
        }
    }

    pub(crate) fn set_local_loadout(&mut self, loadout: crate::cosmetics::Loadout) {
        if loadout != self.local_loadout() {
            self.settings.avatar_skin = loadout.avatar_skin;
            self.settings.knife_finish = loadout.knife_finish;
            self.save_settings();
        }
    }

    fn save_settings(&mut self) {
        self.dirty = true;
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            if let Some(directory) = self.settings_path.parent() {
                fs::create_dir_all(directory)?;
            }
            fs::write(
                &self.settings_path,
                serde_json::to_vec_pretty(&self.settings)?,
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            self.set_status(format!("Could not save settings: {error}"));
        }
    }

    fn record_finished_run(&mut self, game: &Game) {
        let run = &game.session;
        if run.phase != RunPhase::Finished
            || self.finished_epoch == Some(game.run_epoch)
            || !run.elapsed.is_finite()
            || run.elapsed <= 0.0
            || game.watching_replay()
            || game.drill_section().is_some()
        {
            return;
        }
        self.finished_epoch = Some(game.run_epoch);
        self.lesson_result = None;
        self.finish_xp = 0;
        if let Some(lesson) = game.tutorial_lesson() {
            if self.training.unlocked(lesson.id) {
                match self
                    .training
                    .record(lesson, run.elapsed, &run.splits, &run.checkpoint_speeds)
                {
                    Ok(result) => self.lesson_result = Some(result),
                    Err(error) => {
                        self.set_status(format!("Could not save lesson progress: {error}"))
                    }
                }
            } else {
                self.set_status("Finish the earlier lessons to save this completion.".into());
            }
            return;
        }
        let name = self.record_key(game);
        self.finish_pb = self.records.get(&name).map(|record| record.seconds);
        if self.page == Page::Playing {
            self.go(Page::Finished);
        }
        if self.finish_pb.is_some_and(|best| best <= run.elapsed) {
            return;
        }
        let previous_xp = self.finish_pb.is_none().then(|| menu_training::xp(self));
        let previous_record = self.records.insert(
            name.clone(),
            BestRun {
                seconds: run.elapsed,
                splits: run.splits.clone(),
                speeds: run.checkpoint_speeds.clone(),
            },
        );
        let path = self.settings_path.with_file_name("records.json");
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            if let Some(directory) = path.parent() {
                fs::create_dir_all(directory)?;
            }
            let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
            fs::write(&temp, serde_json::to_vec_pretty(&self.records)?)?;
            if let Err(error) = fs::rename(&temp, path) {
                let _ = fs::remove_file(temp);
                return Err(error.into());
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Some(previous) = previous_record {
                self.records.insert(name, previous);
            } else {
                self.records.remove(&name);
            }
            self.set_status(format!("Could not save the personal best: {error}"));
        } else if let Some(previous) = previous_xp {
            self.finish_xp = menu_training::xp(self).saturating_sub(previous);
        }
    }

    fn record_key(&self, game: &Game) -> String {
        let options = game.session.options();
        let mut key = game.course.name.clone();
        if game.course.catalog_revision.is_none()
            && (bevy_surf::catalog::find(&key).is_some() || key.eq_ignore_ascii_case("Downline"))
        {
            key.push_str(" · Local map");
        }
        if let Some(number) = game.selected_bonus() {
            key.push_str(&format!(" · Bonus {number}"));
        }
        if !options.auto_bhop {
            key.push_str(" · Manual bhop");
        }
        if options.unrestricted_start {
            key.push_str(" · Unrestricted start");
        }
        key
    }

    fn commit_edit(&mut self) {
        let Some((field, value)) = self.edit.take() else {
            return;
        };
        self.dirty = true;
        let number = value
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|number| number.is_finite());
        let result = match field {
            SettingsField::Search => {
                self.settings_search = value;
                return;
            }
            SettingsField::Sensitivity => value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|n| (0.0..=20.0).contains(n))
                .map(|n| self.settings.sensitivity = n)
                .ok_or("Enter a sensitivity from 0 to 20."),
            SettingsField::Fov => number
                .filter(|n| (50.0..=130.0).contains(n))
                .map(|n| self.settings.fov = n)
                .ok_or("Enter a field of view from 50° to 130°."),
            SettingsField::Volume => number
                .filter(|n| (0.0..=100.0).contains(n))
                .map(|n| self.settings.environmental_volume = n / 100.0)
                .ok_or("Enter a volume from 0% to 100%."),
            SettingsField::AlternateYaw => number
                .filter(|n| valid_yaw_rate(*n))
                .map(|n| self.settings.alternate_yaw = n)
                .ok_or("Enter a yaw speed between −100000 and 100000."),
            SettingsField::AddYaw => {
                if let Some(n) = number.filter(|n| valid_yaw_rate(*n)) {
                    if let Some(index) = self.settings.yaw_speeds.iter().position(|rate| *rate == n)
                    {
                        self.settings.yaw_index = index;
                        Ok(())
                    } else if self.settings.yaw_speeds.len() < 8 {
                        self.settings.yaw_speeds.push(n);
                        self.settings.yaw_index = self.settings.yaw_speeds.len() - 1;
                        Ok(())
                    } else {
                        Err("You can save up to eight yaw speeds.")
                    }
                } else {
                    Err("Enter a yaw speed between −100000 and 100000.")
                }
            }
            SettingsField::CrosshairColor => parse_rgb(&value)
                .map(|rgb| self.settings.crosshair.rgb = rgb)
                .ok_or("Enter a six-digit hex colour."),
            SettingsField::CrosshairCode => {
                CrosshairStyle::from_code(&value).map(|style| self.settings.crosshair = style)
            }
        };
        match result {
            Ok(()) => {
                self.set_status(String::new());
                self.save_settings();
            }
            Err(error) => {
                self.edit = Some((field, value));
                self.set_status(error.into());
            }
        }
    }
}

#[derive(Clone)]
enum Action {
    Open(Page),
    Request(MenuRequest),
    Resume,
    Restart,
    ReturnFromLessonDemo,
    PracticeLessonSection(usize),
    ExitLessonDrill,
    HideStarterGuide,
    Practice(Option<usize>),
    SavePosition,
    RestorePosition,
    Bonus(Option<u16>),
    Load(usize),
    BrowseLocalMaps,
    MapMode(MapMode),
    MapPage(isize),
    SpawnPage(isize),
    RecordPage(isize),
    ClearSearch,
    Refresh,
    Sensitivity(f64),
    Fov(f32),
    AdjustHud(f32),
    AdjustVolume(f32),
    Toggle(Toggle),
    SettingsTab(SettingsTab),
    Edit(SettingsField),
    CommitEdit,
    Capture(ControlAction, Option<usize>),
    ResetBindings,
    SelectYaw(usize),
    RemoveYaw(usize),
    CrosshairPreset(usize),
    CrosshairAdjust(CrosshairPart, i8),
    CopyCrosshair,
    PasteCrosshair,
    OnlineRecords(bool),
    EquipAvatar(AvatarSkin),
    EquipKnife(KnifeFinish),
    ResetSettings,
    Quit,
}

#[derive(Component)]
struct MenuButton(Action);

#[derive(Component)]
struct HoverSurface {
    idle: Color,
    hover: Color,
}

#[derive(Component)]
pub(super) struct UiRoot;

#[derive(Component)]
pub(super) enum Label {
    Notice,
    Clock,
    Mode,
    Best,
    Speed,
    StageProgress,
    RunStatus,
    Sync,
    Route(usize),
    Splits,
    Diagnostics,
    Chat,
    RoomMapClock,
    RoomVoteClock,
    Ranked,
    Ghost,
    Shortcut,
    LessonHeading,
    LessonCue {
        briefing: String,
        sections: Box<[String]>,
        live: Option<Box<[String; 5]>>,
    },
    LessonHint(String),
    StarterStage,
    StarterCue(Box<[String; 3]>),
    StarterRetry(String),
}

#[derive(Component, Clone, Copy)]
pub(super) struct KeyCap(ControlAction);

#[derive(Component)]
pub(super) struct MenuScroll;

pub fn keyboard(
    mut menu: ResMut<MenuState>,
    game: Res<Game>,
    library: Res<MapLibrary>,
    online: Res<Online>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut input: MessageReader<KeyboardInput>,
    scroll: Res<AccumulatedMouseScroll>,
    mut panes: Query<(&mut ScrollPosition, Option<&RelativeCursorPosition>), With<MenuScroll>>,
) {
    if matches!(
        menu.page,
        Page::Settings
            | Page::Training
            | Page::Friends
            | Page::Profile
            | Page::Servers
            | Page::Pause
            | Page::Store
    ) && menu.capture.is_none()
        && menu.edit.is_none()
        && scroll.delta.y != 0.0
    {
        for (mut pane, cursor) in &mut panes {
            if menu.page == Page::Pause && !cursor.is_some_and(RelativeCursorPosition::cursor_over)
            {
                continue;
            }
            pane.0.y = (pane.0.y - scroll.delta.y.clamp(-3.0, 3.0) * 38.0).max(0.0);
            if menu.page == Page::Settings {
                menu.settings_scroll_y = pane.0.y;
            } else if menu.page != Page::Pause {
                menu.content_scroll_y = pane.0.y;
            }
        }
    }
    if online.is_typing() {
        input.clear();
        return;
    }
    if let Some((action, slot)) = menu.capture {
        let key = input
            .read()
            .find(|event| event.state == ButtonState::Pressed && !event.repeat)
            .map(|event| event.key_code);
        if key == Some(KeyCode::Escape) {
            menu.capture = None;
            menu.dirty = true;
        } else if key == Some(KeyCode::Backspace) {
            if let Some(slot) = slot {
                menu.settings.clear_binding(action, slot);
            }
            menu.capture = None;
            menu.save_settings();
        } else if let Some(binding) = key
            .map(Binding::key)
            .or_else(|| {
                mouse
                    .get_just_pressed()
                    .next()
                    .map(|button| Binding::Mouse((*button).into()))
            })
            .or_else(|| Binding::wheel(scroll.delta))
        {
            menu.settings.set_binding(action, slot, binding);
            menu.capture = None;
            menu.save_settings();
        }
        input.clear();
        return;
    }
    if let Some((field, mut value)) = menu.edit.take() {
        let mut submit = false;
        let mut cancel = false;
        let mut changed = false;
        for event in input.read() {
            if event.state != ButtonState::Pressed {
                continue;
            }
            match event.key_code {
                KeyCode::Escape => {
                    cancel = true;
                    break;
                }
                KeyCode::Enter | KeyCode::NumpadEnter => {
                    submit = true;
                    break;
                }
                KeyCode::Backspace => {
                    changed |= value.pop().is_some();
                }
                KeyCode::KeyA
                    if keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]) =>
                {
                    changed |= !value.is_empty();
                    value.clear();
                }
                _ => {
                    if let Some(text) = &event.text {
                        for character in text.chars().filter(|character| !character.is_control()) {
                            if value.len() + character.len_utf8() <= 64 {
                                value.push(character);
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
        if !cancel {
            if field == SettingsField::Search && changed {
                menu.settings_search.clone_from(&value);
                menu.settings_scroll_y = 0.0;
            }
            menu.edit = Some((field, value));
            if submit {
                menu.actions.push(Action::CommitEdit);
            }
        }
        menu.dirty |= cancel || changed;
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        if menu.page == Page::Finished && game.drill_section().is_some() {
            menu.actions.push(Action::ExitLessonDrill);
            input.clear();
            return;
        }
        let page = match menu.page {
            Page::Playing => Page::Pause,
            Page::Pause if game.drill_result().is_some() => Page::Finished,
            Page::Pause => Page::Playing,
            Page::Finished => Page::Pause,
            Page::Home => Page::Home,
            _ => menu.home_page,
        };
        menu.go(page);
        input.clear();
        return;
    }
    if keys.just_pressed(KeyCode::Enter)
        && matches!(menu.page, Page::Home | Page::Pause)
        && game.pending.is_none()
    {
        let action = if menu.page == Page::Home {
            Action::Request(MenuRequest::QuickPlay)
        } else {
            Action::Resume
        };
        menu.actions.push(action);
    }
    if keys.just_pressed(KeyCode::Enter) && menu.page == Page::Finished {
        menu.actions.push(Action::Restart);
    }
    if menu.page == Page::Playing {
        if menu
            .settings
            .just_pressed(ControlAction::Diagnostics, &keys, &mouse, scroll.delta)
        {
            menu.actions.push(Action::Toggle(Toggle::Diagnostics));
        }
        input.clear();
        return;
    }
    for (key, page) in [
        (KeyCode::F1, Page::Home),
        (KeyCode::F2, Page::Maps),
        (KeyCode::F3, Page::Servers),
        (KeyCode::F4, Page::Practice),
        (KeyCode::F5, Page::Settings),
        (KeyCode::F6, Page::Records),
        (KeyCode::F7, Page::Training),
    ] {
        if keys.just_pressed(key) {
            menu.actions.push(Action::Open(page));
            input.clear();
            return;
        }
    }
    if menu.page != Page::Maps {
        input.clear();
        return;
    }
    if keys.just_pressed(KeyCode::Enter) && !menu.query.trim().is_empty() {
        let matches = filtered_maps(&library, &menu);
        if let [(index, _)] = matches.as_slice() {
            menu.actions.push(Action::Load(*index));
        }
    }
    if keys.just_pressed(KeyCode::ArrowRight) || scroll.delta.y < 0.0 {
        menu.actions.push(Action::MapPage(1));
    } else if keys.just_pressed(KeyCode::ArrowLeft) || scroll.delta.y > 0.0 {
        menu.actions.push(Action::MapPage(-1));
    }
    for event in input.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }
        if event.key_code == KeyCode::Backspace {
            if menu.query.pop().is_some() {
                menu.map_page = 0;
                menu.dirty = true;
            }
        } else if let Some(text) = &event.text {
            for character in text.chars().filter(|character| !character.is_control()) {
                if menu.query.len() < 64 {
                    menu.query.push(character);
                    menu.map_page = 0;
                    menu.dirty = true;
                }
            }
        }
    }
}

pub fn apply_actions(
    mut menu: ResMut<MenuState>,
    mut game: ResMut<Game>,
    mut library: ResMut<MapLibrary>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    mut exit: MessageWriter<AppExit>,
    mut requests: MessageWriter<MenuRequest>,
    mut clipboard: ResMut<Clipboard>,
    mut clipboard_read: Local<Option<ClipboardRead>>,
) {
    if game.session.phase == RunPhase::Finished
        && menu.finished_epoch != Some(game.run_epoch)
        && !game.watching_replay()
        && game.drill_section().is_none()
    {
        menu.record_finished_run(&game);
    }
    let previous_options = crate::view::run_options(&menu.settings);
    if let Some(result) = clipboard_read.as_mut().and_then(ClipboardRead::poll_result) {
        *clipboard_read = None;
        match result {
            Ok(value) if menu.page == Page::Settings && value.len() <= 64 => {
                menu.edit = Some((SettingsField::CrosshairCode, value));
                menu.commit_edit();
            }
            Ok(_) if menu.page == Page::Settings => {
                menu.set_status("Crosshair code is too long.".into())
            }
            Err(error) => menu.set_status(format!("Could not read crosshair code: {error}")),
            _ => {}
        }
    }
    let actions = std::mem::take(&mut menu.actions);
    for action in actions {
        match action {
            Action::Open(Page::Practice) if game.drill_section().is_some() => {}
            Action::Open(Page::Maps) => menu.browse_maps(false),
            Action::Open(page) => {
                if page == Page::Practice {
                    menu.spawn_page = 0;
                }
                menu.go(page);
            }
            Action::BrowseLocalMaps => menu.browse_maps(true),
            Action::MapMode(mode) => {
                menu.map_mode = mode;
                menu.map_page = 0;
                menu.reset_content_scroll();
                menu.refresh();
            }
            Action::Request(MenuRequest::StartLesson(id) | MenuRequest::WatchLesson(id))
                if game.pending.is_some() || !menu.training.unlocked(id) => {}
            Action::Request(MenuRequest::StartPathCourse(id))
                if game.pending.is_some() || !menu_training::path_course_available(&menu, id) => {}
            Action::Request(request) => {
                if matches!(
                    request,
                    MenuRequest::StartLesson(_) | MenuRequest::WatchLesson(_)
                ) {
                    let _ = game.exit_lesson_drill();
                }
                requests.write(request);
            }
            Action::Resume if game.pending.is_none() => menu.go(if game.drill_result().is_some() {
                Page::Finished
            } else {
                Page::Playing
            }),
            Action::ReturnFromLessonDemo => {
                if let Some(page) = game.exit_lesson_demo() {
                    menu.go(page);
                    menu.refresh();
                }
            }
            Action::PracticeLessonSection(index)
                if matches!(menu.page, Page::Pause | Page::Finished)
                    && game.pending.is_none()
                    && game
                        .tutorial_lesson()
                        .is_some_and(|lesson| menu.training.unlocked(lesson.id)) =>
            {
                match game.practice_lesson_section(index, menu.page) {
                    Ok(()) => {
                        menu.go(Page::Finished);
                        menu.refresh();
                    }
                    Err(error) => menu.set_status(error.into()),
                }
            }
            Action::ExitLessonDrill => {
                if let Some(page) = game.exit_lesson_drill() {
                    menu.go(page);
                    menu.refresh();
                }
            }
            Action::HideStarterGuide => {
                game.hide_starter_guide();
                menu.refresh();
            }
            Action::Restart if game.pending.is_none() => {
                game.restart();
                menu.go(Page::Playing);
            }
            Action::Practice(_)
            | Action::SavePosition
            | Action::RestorePosition
            | Action::Bonus(_)
                if game.drill_section().is_some() => {}
            Action::Practice(spawn) if game.pending.is_none() => {
                game.set_practice(spawn);
                menu.go(Page::Playing);
            }
            Action::SavePosition => {
                game.save_position();
                menu.set_status("Position saved.".into());
                menu.dirty = true;
            }
            Action::RestorePosition => {
                if game.restore_position() {
                    menu.go(Page::Playing);
                } else {
                    menu.set_status("Save a position first.".into());
                }
            }
            Action::Bonus(number) if game.pending.is_none() => {
                if game.set_bonus(number) {
                    menu.go(Page::Playing);
                }
            }
            Action::Load(index) if game.pending.is_none() => {
                if let Some(map) = library.entries.get(index) {
                    match library.start_load(map.source.clone()) {
                        Ok(job) => {
                            requests.write(MenuRequest::EnterLocalMap);
                            game.pending = Some(job);
                            menu.set_status(format!("Opening {}…", map.name));
                        }
                        Err(error) => menu.set_status(error),
                    }
                    menu.dirty = true;
                }
            }
            Action::MapPage(delta) => {
                let max = filtered_maps(&library, &menu)
                    .len()
                    .div_ceil(MAPS_PER_PAGE)
                    .saturating_sub(1);
                let next = menu.map_page.saturating_add_signed(delta).min(max);
                if next != menu.map_page {
                    menu.map_page = next;
                    menu.dirty = true;
                }
            }
            Action::SpawnPage(delta) => {
                let max = (game.course.practice_spawns.len() + game.course.bonus_routes.len())
                    .div_ceil(SPAWNS_PER_PAGE)
                    .saturating_sub(1);
                menu.spawn_page = menu.spawn_page.saturating_add_signed(delta).min(max);
                menu.dirty = true;
            }
            Action::RecordPage(delta) => {
                let max = menu
                    .records
                    .len()
                    .div_ceil(SPAWNS_PER_PAGE)
                    .saturating_sub(1);
                menu.record_page = menu.record_page.saturating_add_signed(delta).min(max);
                menu.dirty = true;
            }
            Action::ClearSearch => {
                menu.query.clear();
                menu.map_page = 0;
                menu.dirty = true;
            }
            Action::Refresh => {
                library.rescan();
                menu.set_status("Local map folders rescanned.".into());
                menu.map_page = 0;
                menu.dirty = true;
            }
            Action::Sensitivity(amount) => {
                menu.settings.sensitivity = (menu.settings.sensitivity + amount).clamp(0.0, 20.0);
                menu.save_settings();
            }
            Action::Fov(amount) => {
                menu.settings.fov = (menu.settings.fov + amount).clamp(50.0, 130.0);
                menu.save_settings();
            }
            Action::AdjustHud(amount) => {
                menu.settings.hud_scale = (menu.settings.hud_scale + amount).clamp(0.7, 1.8);
                menu.save_settings();
            }
            Action::AdjustVolume(amount) => {
                menu.settings.environmental_volume =
                    (menu.settings.environmental_volume + amount).clamp(0.0, 1.0);
                menu.save_settings();
            }
            Action::Toggle(toggle) => {
                let settings = &mut menu.settings;
                let value = match toggle {
                    Toggle::Keys => &mut settings.show_keys,
                    Toggle::Crosshair => &mut settings.show_crosshair,
                    Toggle::Vsync => &mut settings.vsync,
                    Toggle::Fullscreen => &mut settings.fullscreen,
                    Toggle::MenuMotion => &mut settings.menu_motion,
                    Toggle::EnvironmentalAudio => &mut settings.environmental_audio,
                    Toggle::InvertMouse => &mut settings.invert_mouse,
                    Toggle::SmoothCamera => &mut settings.smooth_camera,
                    Toggle::AutoBhop => &mut settings.auto_bhop,
                    Toggle::UnrestrictedStart => &mut settings.unrestricted_start,
                    Toggle::Turns => &mut settings.turns_enabled,
                    Toggle::CenterSpeed => &mut settings.center_speed,
                    Toggle::StrafeSync => &mut settings.show_sync,
                    Toggle::CompareSplits => &mut settings.compare_splits,
                    Toggle::CompareSpeed => &mut settings.compare_speed,
                    Toggle::Colorblind => &mut settings.colorblind,
                    Toggle::Diagnostics => &mut settings.diagnostics,
                    Toggle::CrosshairT => &mut settings.crosshair.t_shape,
                };
                *value = !*value;
                menu.save_settings();
            }
            Action::SettingsTab(tab) => {
                menu.settings_tab = tab;
                menu.settings_search.clear();
                menu.settings_scroll_y = 0.0;
                menu.edit = None;
                menu.capture = None;
                menu.dirty = true;
            }
            Action::Edit(field) => {
                menu.capture = None;
                let value = match field {
                    SettingsField::Search => menu.settings_search.clone(),
                    _ => String::new(),
                };
                menu.edit = Some((field, value));
                menu.dirty = true;
            }
            Action::CommitEdit => menu.commit_edit(),
            Action::Capture(action, slot) => {
                menu.edit = None;
                menu.capture = Some((action, slot));
                menu.dirty = true;
            }
            Action::ResetBindings => {
                menu.capture = None;
                menu.settings.reset_bindings();
                menu.save_settings();
            }
            Action::SelectYaw(index) => {
                menu.settings.yaw_index = index.min(menu.settings.yaw_speeds.len() - 1);
                menu.save_settings();
            }
            Action::RemoveYaw(index) => {
                if menu.settings.yaw_speeds.len() > 1 && index < menu.settings.yaw_speeds.len() {
                    menu.settings.yaw_speeds.remove(index);
                    if index < menu.settings.yaw_index {
                        menu.settings.yaw_index -= 1;
                    }
                    menu.settings.yaw_index = menu
                        .settings
                        .yaw_index
                        .min(menu.settings.yaw_speeds.len() - 1);
                    menu.save_settings();
                }
            }
            Action::CrosshairPreset(index) => {
                menu.settings.crosshair = CrosshairStyle::preset(index);
                menu.save_settings();
            }
            Action::CrosshairAdjust(part, amount) => {
                menu.settings.crosshair.adjust(part, amount);
                menu.save_settings();
            }
            Action::CopyCrosshair => match clipboard.set_text(menu.settings.crosshair.code()) {
                Ok(()) => menu.set_status("Crosshair code copied.".into()),
                Err(error) => menu.set_status(format!("Could not copy crosshair code: {error}")),
            },
            Action::PasteCrosshair => *clipboard_read = Some(clipboard.fetch_text()),
            Action::OnlineRecords(value) => {
                menu.show_online_records = value;
                menu.dirty = true;
            }
            Action::EquipAvatar(skin) => {
                requests.write(MenuRequest::Equip(crate::online::Equip::Avatar(skin)));
            }
            Action::EquipKnife(finish) => {
                requests.write(MenuRequest::Equip(crate::online::Equip::Knife(finish)));
            }
            Action::ResetSettings => {
                let (avatar_skin, knife_finish) =
                    (menu.settings.avatar_skin, menu.settings.knife_finish);
                menu.settings = Settings::default();
                menu.settings.avatar_skin = avatar_skin;
                menu.settings.knife_finish = knife_finish;
                menu.save_settings();
            }
            Action::Quit => {
                exit.write(AppExit::Success);
            }
            _ => {}
        }
    }
    let options = crate::view::run_options(&menu.settings);
    if options != previous_options {
        game.set_run_options(options);
    }
    let present_mode = if menu.settings.vsync {
        PresentMode::AutoVsync
    } else {
        PresentMode::AutoNoVsync
    };
    if window.present_mode != present_mode {
        window.present_mode = present_mode;
    }
    let mode = if menu.settings.fullscreen {
        WindowMode::BorderlessFullscreen(MonitorSelection::Current)
    } else {
        WindowMode::Windowed
    };
    if window.mode != mode {
        window.mode = mode;
    }
    let page = match menu.page {
        Page::Home => "Home",
        Page::Maps => "Maps",
        Page::Practice => "Practice",
        Page::Training => "Surf School",
        Page::Servers => "Servers",
        Page::Players => "Players",
        Page::Records => "Records",
        Page::Profile => "Profile & loadout",
        Page::Friends => "Friends",
        Page::Store => "Store",
        Page::Moderation => "Moderation",
        Page::Settings => "Settings",
        Page::Pause => "Paused",
        Page::Playing => game
            .tutorial_lesson()
            .map_or(game.course.name.as_str(), |lesson| lesson.name),
        Page::Finished => "Run complete",
    };
    if window.title.strip_prefix("Surf / Bevy — ") != Some(page) {
        window.title = format!("Surf / Bevy — {page}");
    }
}

#[derive(Default)]
pub(crate) struct PauseRenderState {
    icons: Option<menu_pause_board::Icons>,
    previous: Option<(menu_pause_board::Layout, bool, bool)>,
}

pub fn render(
    mut commands: Commands,
    mut menu: ResMut<MenuState>,
    game: Res<Game>,
    library: Res<MapLibrary>,
    online: Res<Online>,
    roots: Query<Entity, With<UiRoot>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut images: ResMut<Assets<Image>>,
    mut previews: Local<BTreeMap<PathBuf, Handle<Image>>>,
    mut mark: Local<Option<Handle<Image>>>,
    mut pause_render: Local<PauseRenderState>,
    mut knife_previews: Local<Option<menu_cosmetics::KnifePreviews>>,
) {
    let board_layout = if menu.page == Page::Pause
        && game.tutorial_lesson().is_none()
        && game.drill_section().is_none()
    {
        let (width, height) = windows
            .single()
            .map_or((1280., 800.), |window| (window.width(), window.height()));
        menu_pause_board::Layout::new(width, height)
    } else {
        None
    };
    let pause_board = board_layout.map(|layout| {
        (
            layout,
            game.ghost_enabled(),
            game.ghost_preparation_progress().is_some(),
        )
    });
    if !menu.dirty && pause_render.previous == pause_board {
        return;
    }
    pause_render.previous = pause_board;
    menu.dirty = false;
    for root in &roots {
        commands.entity(root).despawn();
    }
    if menu.page == Page::Playing {
        spawn_hud(&mut commands, &game, &menu.settings, &online);
        return;
    }
    let mark = mark.get_or_insert_with(|| brand_image(&mut images)).clone();

    commands
        .spawn((
            UiRoot,
            Node {
                width: percent(100),
                height: percent(100),
                min_height: px(0),
                align_items: if matches!(menu.page, Page::Home | Page::Pause) {
                    AlignItems::Start
                } else {
                    AlignItems::Center
                },
                justify_content: JustifyContent::Center,
                flex_direction: FlexDirection::Column,
                padding: UiRect::axes(percent(5), px(20)),
                overflow: Overflow::clip(),
                ..default()
            },
        ))
        .with_children(|outer| {
            if menu.page == Page::Home {
                outer.spawn((
                    BackdropImage,
                    ImageNode::default(),
                    Visibility::Hidden,
                    Pickable::IGNORE,
                    backdrop_node(),
                ));
            }
            outer.spawn((
                backdrop_node(),
                Pickable::IGNORE,
                BackgroundGradient(vec![
                    Gradient::Linear(LinearGradient::to_right(vec![
                        ColorStop::new(Color::srgba_u8(7, 12, 15, 224), percent(0)),
                        ColorStop::new(Color::srgba_u8(7, 12, 15, 173), percent(25)),
                        ColorStop::new(Color::srgba_u8(7, 12, 15, 77), percent(52)),
                        ColorStop::new(Color::srgba_u8(7, 12, 15, 46), percent(100)),
                    ])),
                    Gradient::Linear(LinearGradient::to_top(vec![
                        ColorStop::new(Color::srgba_u8(7, 12, 15, 184), percent(0)),
                        ColorStop::new(Color::NONE, percent(24)),
                        ColorStop::new(Color::NONE, percent(80)),
                        ColorStop::new(Color::srgba_u8(7, 12, 15, 128), percent(100)),
                    ])),
                ]),
            ));
            if matches!(menu.page, Page::Home | Page::Pause) {
                if menu.page == Page::Home {
                    outer.spawn((
                        hud_corner(None, None, Some(28.), Some(66.)),
                        BackdropCaption,
                        text("", 12., MUTED),
                        Pickable::IGNORE,
                    ));
                }
                home(
                    outer,
                    &menu,
                    &game,
                    &library,
                    &online,
                    mark,
                    board_layout.map_or(520., |layout| layout.left_width),
                );
                if let Some(layout) = board_layout {
                    let icons = pause_render
                        .icons
                        .get_or_insert_with(|| menu_pause_board::Icons::new(&mut images));
                    menu_pause_board::render(outer, &menu, &game, &online, layout, icons);
                }
                return;
            }
            if menu.page == Page::Finished {
                if let (Some(section), Some(result), Some(return_page)) = (
                    game.drill_section(),
                    game.drill_result(),
                    game.drill_return_page(),
                ) {
                    let velocity = game.session.player.velocity;
                    menu_training::drill_finish(
                        outer,
                        &menu,
                        section,
                        result,
                        velocity.x.hypot(velocity.z),
                        return_page,
                    );
                } else if let Some(lesson) = game.tutorial_lesson() {
                    menu_training::finish(outer, &menu, lesson, game.session.elapsed);
                } else {
                    finish(outer, &menu, &game);
                }
                return;
            }
            outer
                .spawn((
                    Node {
                        width: percent(100),
                        max_width: px(900),
                        max_height: percent(100),
                        min_height: px(0),
                        flex_direction: FlexDirection::Column,
                        border: UiRect::all(px(1)),
                        ..default()
                    },
                    BackgroundColor(PANEL),
                    BorderColor::all(EDGE),
                ))
                .with_children(|window| {
                    titlebar(window, &menu);
                    let scrollable = matches!(
                        menu.page,
                        Page::Training
                            | Page::Friends
                            | Page::Profile
                            | Page::Servers
                            | Page::Store
                    );
                    let mut contents = window.spawn(Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: px(11),
                        padding: UiRect::all(px(15)),
                        min_height: px(0),
                        overflow: if scrollable {
                            Overflow::scroll_y()
                        } else {
                            Overflow::clip()
                        },
                        ..default()
                    });
                    if scrollable {
                        contents.insert((
                            MenuScroll,
                            ScrollPosition(Vec2::new(0., menu.content_scroll_y)),
                        ));
                    }
                    contents.with_children(|layout| {
                        match menu.page {
                            Page::Maps => map_library(
                                layout,
                                &menu,
                                &game,
                                &library,
                                &mut images,
                                &mut previews,
                            ),
                            Page::Practice => practice(layout, &menu, &game),
                            Page::Training => menu_training::render(layout, &menu, &game),
                            Page::Servers => online_ui::panel(
                                layout,
                                OnlinePage::Servers,
                                &online,
                                &game.course.name,
                            ),
                            Page::Players => online_ui::panel(
                                layout,
                                OnlinePage::Players,
                                &online,
                                &game.course.name,
                            ),
                            Page::Records => {
                                layout
                                    .spawn(Node {
                                        column_gap: px(8),
                                        ..default()
                                    })
                                    .with_children(|tabs| {
                                        button(
                                            tabs,
                                            "Verified records",
                                            Action::OnlineRecords(true),
                                            menu.show_online_records,
                                        );
                                        button(
                                            tabs,
                                            "Local bests",
                                            Action::OnlineRecords(false),
                                            !menu.show_online_records,
                                        );
                                    });
                                if menu.show_online_records {
                                    online_ui::panel(
                                        layout,
                                        OnlinePage::Records,
                                        &online,
                                        &game.course.name,
                                    );
                                } else {
                                    records(layout, &menu);
                                }
                            }
                            Page::Profile => {
                                if online.viewed_profile.is_none()
                                    && online.profile_runs.is_none()
                                    && !online.web_history.visible
                                {
                                    link(
                                        layout,
                                        "Customize skins and knife",
                                        Action::Open(Page::Store),
                                    );
                                }
                                online_ui::panel(
                                    layout,
                                    OnlinePage::Profile,
                                    &online,
                                    &game.course.name,
                                );
                            }
                            Page::Friends => online_ui::panel(
                                layout,
                                OnlinePage::Friends,
                                &online,
                                &game.course.name,
                            ),
                            Page::Store => menu_cosmetics::render(
                                layout,
                                &menu.settings,
                                &online,
                                knife_previews.get_or_insert_with(|| {
                                    menu_cosmetics::KnifePreviews::new(&mut images)
                                }),
                            ),
                            Page::Moderation => online_ui::panel(
                                layout,
                                OnlinePage::Moderation,
                                &online,
                                &game.course.name,
                            ),
                            Page::Settings => settings(layout, &menu, &game),
                            Page::Home | Page::Pause | Page::Playing | Page::Finished => {
                                unreachable!()
                            }
                        }
                        layout.spawn((text(&menu.notice, 12., MINT), Label::Notice));
                    });
                });
        });
}

fn backdrop_node() -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: px(0),
        top: px(0),
        width: percent(100),
        height: percent(100),
        ..default()
    }
}

fn titlebar(parent: &mut ChildSpawnerCommands, menu: &MenuState) {
    let title = match menu.page {
        Page::Maps | Page::Practice => "Solo & practice",
        Page::Training => "Surf School",
        Page::Servers => "Find a server",
        Page::Players => "Players",
        Page::Records => "Leaderboards",
        Page::Profile => "Profile & loadout",
        Page::Friends => "Friends",
        Page::Store => "Store",
        Page::Moderation => "Moderation",
        Page::Settings => "Settings",
        _ => unreachable!(),
    };
    parent
        .spawn((
            Node {
                width: percent(100),
                padding: UiRect::axes(px(12), px(4)),
                border: UiRect::bottom(px(1)),
                align_items: AlignItems::Center,
                column_gap: px(8),
                ..default()
            },
            BorderColor::all(EDGE),
            BackgroundColor(TILE),
        ))
        .with_children(|header| {
            header.spawn((
                Node {
                    flex_grow: 1.0,
                    ..default()
                },
                text(title, 14., INK),
            ));
            if menu.page == Page::Settings {
                header.spawn(text("Changes save automatically", 11., MUTED));
            }
            button(header, "×", Action::Open(menu.home_page), false);
        });
}

fn home(
    parent: &mut ChildSpawnerCommands,
    menu: &MenuState,
    game: &Game,
    library: &MapLibrary,
    online: &Online,
    mark: Handle<Image>,
    max_width: f32,
) {
    let paused = menu.page == Page::Pause;
    let connected = online.is_connected();
    parent
        .spawn(Node {
            width: percent(100),
            max_width: px(max_width),
            flex_direction: FlexDirection::Column,
            row_gap: px(8),
            ..default()
        })
        .with_children(|stack| {
            stack
                .spawn(Node {
                    align_items: AlignItems::Center,
                    column_gap: px(13),
                    margin: UiRect::bottom(px(23)),
                    ..default()
                })
                .with_children(|brand| {
                    brand.spawn((
                        ImageNode::new(mark),
                        Node {
                            width: px(64),
                            height: px(38),
                            ..default()
                        },
                    ));
                    brand.spawn(text("surfd", 52., INK));
                    brand.spawn((
                        Node {
                            margin: UiRect::left(px(-11)),
                            ..default()
                        },
                        text(".net", 52., MINT),
                    ));
                });
            let play = if paused {
                Action::Resume
            } else {
                Action::Request(MenuRequest::QuickPlay)
            };
            stack
                .spawn((
                    MenuButton(play),
                    Node {
                        width: percent(100),
                        padding: UiRect::axes(px(18), px(12)),
                        border: UiRect::all(px(2)),
                        flex_direction: FlexDirection::Column,
                        row_gap: px(5),
                        ..default()
                    },
                    BorderColor::all(EDGE),
                    BackgroundGradient(vec![Gradient::Linear(LinearGradient::to_bottom(vec![
                        ColorStop::auto(Color::srgba_u8(52, 92, 104, 245)),
                        ColorStop::auto(Color::srgba_u8(30, 56, 66, 245)),
                    ]))]),
                ))
                .observe(click)
                .with_children(|card| {
                    card.spawn((
                        text(if paused { "Resume" } else { "Play now" }, 34., INK),
                        Pickable::IGNORE,
                    ));
                    let detail = if paused {
                        let run = &game.session;
                        if run.phase == RunPhase::Running {
                            format!(
                                "{}  /  {}    Esc",
                                game.course.name,
                                format_time(run.elapsed)
                            )
                        } else {
                            format!("{}    Esc", game.course.name)
                        }
                    } else if connected {
                        "Current server".into()
                    } else {
                        format!("{}  /  Solo available", game.course.name)
                    };
                    card.spawn((text(detail, 13., MINT), Pickable::IGNORE));
                });
            stack
                .spawn(Node {
                    width: percent(100),
                    justify_content: JustifyContent::SpaceBetween,
                    align_items: AlignItems::Center,
                    ..default()
                })
                .with_children(|below| {
                    if paused {
                        let credit = library
                            .entries
                            .iter()
                            .find(|entry| {
                                entry.name == game.course.name || entry.id == game.course.name
                            })
                            .map_or("Source-style surf".into(), |entry| entry.detail.clone());
                        below.spawn(text(credit, 12., MUTED));
                        link(
                            below,
                            &format!(
                                "{}  {}",
                                if game.drill_section().is_some() {
                                    "Restart section"
                                } else {
                                    "Restart run"
                                },
                                menu.settings.primary_binding(ControlAction::Restart)
                            ),
                            Action::Restart,
                        );
                    } else {
                        checkbox_link(
                            below,
                            "Fullscreen",
                            menu.settings.fullscreen,
                            Action::Toggle(Toggle::Fullscreen),
                        );
                        link(
                            below,
                            "Choose a different server",
                            Action::Open(Page::Servers),
                        );
                    }
                });
            if paused
                && game.drill_section().is_none()
                && (!game.course.practice_spawns.is_empty() || !game.course.bonus_routes.is_empty())
            {
                stack
                    .spawn(Node {
                        column_gap: px(8),
                        flex_wrap: FlexWrap::Wrap,
                        align_items: AlignItems::Center,
                        ..default()
                    })
                    .with_children(|course| {
                        if !game.course.bonus_routes.is_empty() {
                            link(course, "Course  /  Bonuses", Action::Open(Page::Practice));
                        }
                        if !game.course.practice_spawns.is_empty() {
                            link(course, "Go to stage", Action::Open(Page::Practice));
                        }
                    });
            }
            if paused && let Some(lesson) = game.tutorial_lesson() {
                if let Some(return_page) = game.drill_return_page() {
                    link(
                        stack,
                        menu_training::drill_return_label(return_page),
                        Action::ExitLessonDrill,
                    );
                } else if game.watching_lesson_demo() {
                    link(stack, "Return to lesson", Action::ReturnFromLessonDemo);
                } else {
                    link(
                        stack,
                        "Watch demo",
                        Action::Request(MenuRequest::WatchLesson(lesson.id)),
                    );
                    let index = game.session.splits.len();
                    if matches!(game.session.phase, RunPhase::Ready | RunPhase::Running)
                        && menu.training.unlocked(lesson.id)
                        && let Some(section) = lesson.sections.get(index)
                    {
                        link(
                            stack,
                            &format!("Practice {}", section.name.to_lowercase()),
                            Action::PracticeLessonSection(index),
                        );
                    }
                }
            }
            if paused && game.starter_guide() {
                link(stack, "Hide guide", Action::HideStarterGuide);
            }
            stack.spawn((text(&menu.notice, 12., MINT), Label::Notice));
            stack
                .spawn(Node {
                    align_items: AlignItems::Start,
                    flex_direction: FlexDirection::Column,
                    row_gap: px(2),
                    ..default()
                })
                .with_children(|verbs| {
                    if connected {
                        verb(verbs, "Players", Action::Open(Page::Players));
                    }
                    verb(verbs, "Find a server", Action::Open(Page::Servers));
                    verb(verbs, "Surf School", Action::Open(Page::Training));
                    verb(verbs, "Solo & practice", Action::Open(Page::Maps));
                    verb(verbs, "Leaderboards", Action::Open(Page::Records));
                    verb(verbs, "Profile & loadout", Action::Open(Page::Profile));
                    verb(verbs, "Store", Action::Open(Page::Store));
                    verb(verbs, "Settings", Action::Open(Page::Settings));
                    if connected {
                        verb(verbs, "Moderation", Action::Open(Page::Moderation));
                        verb(
                            verbs,
                            "Leave server",
                            Action::Request(MenuRequest::LeaveServer),
                        );
                    }
                });
        });
    parent
        .spawn(hud_corner(None, Some(26.), None, Some(18.)))
        .with_children(|dock| {
            link(dock, "Quit game", Action::Quit);
        });
    parent
        .spawn(hud_corner(None, None, Some(26.), Some(18.)))
        .with_children(|dock| {
            button(dock, "Friends", Action::Open(Page::Friends), false);
        });
}

fn brand_image(images: &mut Assets<Image>) -> Handle<Image> {
    const MARK: [[[f32; 2]; 4]; 2] = [
        [[4., 36.], [27., 10.], [43., 10.], [20., 36.]],
        [[25., 36.], [48., 10.], [60., 10.], [37., 36.]],
    ];
    const SCALE: u32 = 4;
    let mut pixels = Vec::with_capacity((64 * 38 * SCALE * SCALE * 4) as usize);
    for y in 0..38 * SCALE {
        for x in 0..64 * SCALE {
            let point = [
                (x as f32 + 0.5) / SCALE as f32,
                (y as f32 + 0.5) / SCALE as f32,
            ];
            let inside = MARK.iter().any(|polygon| {
                polygon
                    .iter()
                    .zip(polygon.iter().cycle().skip(1))
                    .all(|(a, b)| {
                        (b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0]) >= 0.0
                    })
            });
            pixels.extend_from_slice(&[165, 216, 219, if inside { 255 } else { 0 }]);
        }
    }
    images.add(Image::new(
        Extent3d {
            width: 64 * SCALE,
            height: 38 * SCALE,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    ))
}

fn finish(parent: &mut ChildSpawnerCommands, menu: &MenuState, game: &Game) {
    let run = &game.session;
    let path = menu_training::finished_path(menu, game);
    parent
        .spawn((
            Node {
                width: percent(100),
                max_width: px(530),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(22)),
                row_gap: px(10),
                align_items: AlignItems::Center,
                border: UiRect::all(px(1)),
                ..default()
            },
            BorderColor::all(EDGE),
            BackgroundColor(PANEL),
        ))
        .with_children(|card| {
            card.spawn(text("Run complete", 17., MINT));
            card.spawn(text(menu.record_key(game), 23., INK));
            card.spawn(text(format_time(run.elapsed), 52., INK));
            let result = match menu.finish_pb {
                Some(previous) if run.elapsed < previous => {
                    format!("Personal best  ·  {:.3}s faster", previous - run.elapsed)
                }
                Some(previous) => format!("Personal best  /  {}", format_time(previous)),
                None => "Personal best".into(),
            };
            card.spawn(text(result, 14., MINT));
            if menu.finish_xp > 0 {
                card.spawn(text(format!("+{} XP", menu.finish_xp), 16., GOLD));
            }
            if let Some((clears, _)) = path {
                card.spawn(text(menu_training::path_summary(clears), 14., MINT));
            }
            if !menu.notice.is_empty() {
                card.spawn((text(&menu.notice, 13., MUTED), Label::Notice));
            }
            card.spawn((text("", 12., MUTED), Label::Ranked));
            if !run.splits.is_empty() {
                card.spawn(text(
                    run.splits
                        .iter()
                        .take(6)
                        .enumerate()
                        .map(|(index, split)| format!("CP {}  {}", index + 1, format_time(*split)))
                        .collect::<Vec<_>>()
                        .join("    "),
                    12.,
                    MUTED,
                ));
            }
            card.spawn(Node {
                justify_content: JustifyContent::Center,
                column_gap: px(8),
                row_gap: px(8),
                flex_wrap: FlexWrap::Wrap,
                ..default()
            })
            .with_children(|buttons| {
                button(buttons, "Menu", Action::Open(Page::Pause), false);
                if let Some((_, Some(next))) = path {
                    menu_training::path_button(buttons, next);
                } else if path.is_some() || game.starter_guide() {
                    button(
                        buttons,
                        "Choose another map",
                        Action::Open(Page::Maps),
                        path.is_some(),
                    );
                }
                if game.selected_bonus().is_some() {
                    button(buttons, "Main course", Action::Bonus(None), false);
                }
                if game.has_replay() {
                    button(
                        buttons,
                        "Watch replay",
                        Action::Request(MenuRequest::WatchReplay),
                        false,
                    );
                    button(
                        buttons,
                        "Export replay",
                        Action::Request(MenuRequest::ExportReplay),
                        false,
                    );
                }
                button(buttons, "Surf again", Action::Restart, path.is_none());
            });
        });
}

fn map_library(
    parent: &mut ChildSpawnerCommands,
    menu: &MenuState,
    game: &Game,
    library: &MapLibrary,
    images: &mut Assets<Image>,
    previews: &mut BTreeMap<PathBuf, Handle<Image>>,
) {
    let matches = filtered_maps(library, menu);
    let pages = matches.len().div_ceil(MAPS_PER_PAGE).max(1);
    let page = menu.map_page.min(pages - 1);
    let next_path = menu_training::next_path_course(menu);
    if menu.local_maps_only {
        title(
            parent,
            "SOLO PRACTICE",
            "Practice maps",
            format!("{} {} maps", matches.len(), menu.map_mode.label()),
        );
    } else {
        title(
            parent,
            "MAP LIBRARY",
            "Choose a course",
            format!("{} {} maps", matches.len(), menu.map_mode.label()),
        );
    }
    parent
        .spawn(Node {
            width: percent(100),
            column_gap: px(8),
            row_gap: px(4),
            flex_wrap: FlexWrap::Wrap,
            align_items: AlignItems::Center,
            ..default()
        })
        .with_children(|row| {
            let search = if menu.query.is_empty() {
                "Type to search maps".into()
            } else {
                format!("Search  /  {}", menu.query)
            };
            row.spawn((
                Node {
                    padding: UiRect::all(px(9)),
                    flex_grow: 1.0,
                    border: UiRect::all(px(1)),
                    ..default()
                },
                BackgroundColor(PANEL),
                BorderColor::all(EDGE),
                text(search, 15., INK),
            ));
            if !menu.query.is_empty() {
                button(row, "Clear", Action::ClearSearch, false);
            }
            for mode in [MapMode::Surf, MapMode::Bhop] {
                button(
                    row,
                    mode.label(),
                    Action::MapMode(mode),
                    menu.map_mode == mode,
                );
            }
            if menu.local_maps_only {
                button(row, "All maps", Action::Open(Page::Maps), false);
            } else {
                button(row, "Practice maps", Action::BrowseLocalMaps, false);
            }
            button(row, "Current course", Action::Open(Page::Practice), false);
            button(row, "Refresh maps", Action::Refresh, false);
            if !menu.local_maps_only
                && let Some(step) = next_path
            {
                menu_training::path_button(row, step);
            }
        });
    let visible = &matches[page * MAPS_PER_PAGE..matches.len().min((page + 1) * MAPS_PER_PAGE)];
    if visible.is_empty() {
        let message = if menu.local_maps_only && menu.query.trim().is_empty() {
            "Add maps with published tiers and authored timer zones, then rescan."
        } else {
            "No maps match this search."
        };
        parent.spawn(text(message, 18., MUTED));
    }
    for group in visible.chunks(3) {
        parent
            .spawn(Node {
                width: percent(100),
                column_gap: px(10),
                ..default()
            })
            .with_children(|row| {
                for &(index, map) in group {
                    let status = map.status(&library.directory);
                    let preview = map
                        .preview()
                        .and_then(|path| preview_image(path, images, previews));
                    let mut tile = row.spawn((
                        Node {
                            width: percent(32.8),
                            min_width: px(0),
                            padding: UiRect::all(px(9)),
                            border: UiRect::all(px(1)),
                            flex_direction: FlexDirection::Column,
                            row_gap: px(4),
                            ..default()
                        },
                        BackgroundColor(PANEL),
                        BorderColor::all(EDGE),
                    ));
                    if game.pending.is_none() {
                        tile.insert(MenuButton(Action::Load(index))).observe(click);
                    }
                    tile.with_children(|content| {
                        if let Some(preview) = preview {
                            content.spawn((
                                ImageNode::new(preview),
                                Node {
                                    width: percent(100),
                                    height: px(96),
                                    ..default()
                                },
                                Pickable::IGNORE,
                            ));
                        } else {
                            content
                                .spawn((
                                    Node {
                                        width: percent(100),
                                        height: px(96),
                                        align_items: AlignItems::Center,
                                        justify_content: JustifyContent::Center,
                                        ..default()
                                    },
                                    BackgroundColor(TILE),
                                    Pickable::IGNORE,
                                ))
                                .with_child(text(
                                    if status == "BUILT IN" {
                                        "DOWNLINE"
                                    } else {
                                        "MAP"
                                    },
                                    18.,
                                    MINT,
                                ));
                        }
                        content.spawn((text(&map.name, 19., INK), Pickable::IGNORE));
                        let tier = map
                            .tier
                            .map_or_else(|| "Tier unknown".into(), |tier| format!("Tier {tier}"));
                        content.spawn((text(tier, 12., MINT), Pickable::IGNORE));
                        content.spawn((text(&map.detail, 12., MUTED), Pickable::IGNORE));
                        content.spawn((text(status, 11., MINT), Pickable::IGNORE));
                    });
                }
            });
    }
    pager(parent, page, pages, Action::MapPage(-1), Action::MapPage(1));
}

fn filtered_maps<'a>(library: &'a MapLibrary, menu: &MenuState) -> Vec<(usize, &'a MapEntry)> {
    let query = menu.query.trim().to_lowercase();
    let tier = query
        .strip_prefix("tier ")
        .and_then(|value| value.parse::<u8>().ok());
    library
        .entries
        .iter()
        .enumerate()
        .filter(|(_, map)| map.tier.is_some())
        .filter(|(_, map)| map.mode == menu.map_mode)
        .filter(|(_, map)| {
            if menu.local_maps_only {
                map.is_local() || map.is_practice_download()
            } else {
                !map.is_practice_download()
            }
        })
        .filter(|(_, map)| {
            query.is_empty()
                || map.id.to_lowercase().contains(&query)
                || map.name.to_lowercase().contains(&query)
                || map.detail.to_lowercase().contains(&query)
                || tier.is_some_and(|tier| map.tier == Some(tier))
        })
        .collect()
}

fn preview_image(
    path: PathBuf,
    images: &mut Assets<Image>,
    cache: &mut BTreeMap<PathBuf, Handle<Image>>,
) -> Option<Handle<Image>> {
    if let Some(image) = cache.get(&path) {
        return Some(image.clone());
    }
    let rgba = image::open(&path).ok()?.thumbnail(320, 144).into_rgba8();
    let (width, height) = rgba.dimensions();
    let image = images.add(Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        rgba.into_raw(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    ));
    cache.insert(path, image.clone());
    Some(image)
}

fn practice(parent: &mut ChildSpawnerCommands, menu: &MenuState, game: &Game) {
    title(
        parent,
        "SOLO PRACTICE",
        "Work on a section",
        format!("Current course  /  {}", game.course.name),
    );
    parent
        .spawn(Node {
            column_gap: px(10),
            row_gap: px(4),
            flex_wrap: FlexWrap::Wrap,
            ..default()
        })
        .with_children(|actions| {
            button(actions, "Main course", Action::Bonus(None), true);
            button(actions, "Choose a map", Action::Open(Page::Maps), false);
            button(actions, "Practice maps", Action::BrowseLocalMaps, false);
            button(actions, "Surf School", Action::Open(Page::Training), false);
            button(actions, "Save position", Action::SavePosition, false);
            button(actions, "Load position", Action::RestorePosition, false);
        });
    parent.spawn(text(
        "Loading a saved position switches the run to practice.",
        12.,
        MUTED,
    ));
    let count = game.course.bonus_routes.len() + game.course.practice_spawns.len();
    if count == 0 {
        parent.spawn(text(
            "This course has no additional practice starts.",
            16.,
            MUTED,
        ));
        return;
    }
    let pages = count.div_ceil(SPAWNS_PER_PAGE);
    let page = menu.spawn_page.min(pages - 1);
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Column,
            row_gap: px(6),
            max_width: px(570),
            ..default()
        })
        .with_children(|list| {
            let bonuses = game.course.bonus_routes.iter().map(|bonus| {
                (
                    format!("{} · Timed bonus", bonus.name),
                    Action::Bonus(Some(bonus.number)),
                )
            });
            let stations = game
                .course
                .practice_spawns
                .iter()
                .enumerate()
                .map(|(index, spawn)| {
                    (
                        format!("{} · Practice", spawn.name),
                        Action::Practice(Some(index)),
                    )
                });
            for (name, action) in bonuses
                .chain(stations)
                .skip(page * SPAWNS_PER_PAGE)
                .take(SPAWNS_PER_PAGE)
            {
                button(list, &name, action, false);
            }
        });
    if pages > 1 {
        pager(
            parent,
            page,
            pages,
            Action::SpawnPage(-1),
            Action::SpawnPage(1),
        );
    }
}

fn records(parent: &mut ChildSpawnerCommands, menu: &MenuState) {
    title(
        parent,
        "LOCAL RECORDS",
        "Personal bests",
        "Completed solo runs on this computer",
    );
    if menu.records.is_empty() {
        parent.spawn(text(
            "Finish a timed course to set your first personal best.",
            17.,
            MUTED,
        ));
        return;
    }
    let pages = menu.records.len().div_ceil(SPAWNS_PER_PAGE);
    let page = menu.record_page.min(pages - 1);
    for (name, best) in menu
        .records
        .iter()
        .skip(page * SPAWNS_PER_PAGE)
        .take(SPAWNS_PER_PAGE)
    {
        parent
            .spawn((
                Node {
                    padding: UiRect::all(px(11)),
                    column_gap: px(12),
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundColor(PANEL),
            ))
            .with_children(|row| {
                row.spawn((
                    Node {
                        flex_grow: 1.0,
                        ..default()
                    },
                    text(name, 17., INK),
                ));
                row.spawn(text(
                    format!("{} checkpoints", best.splits.len()),
                    14.,
                    MUTED,
                ));
                row.spawn(text(format_time(best.seconds), 17., MINT));
            });
    }
    if pages > 1 {
        pager(
            parent,
            page,
            pages,
            Action::RecordPage(-1),
            Action::RecordPage(1),
        );
    }
}

fn setting_row(
    parent: &mut ChildSpawnerCommands,
    name: &str,
    buttons: impl FnOnce(&mut ChildSpawnerCommands),
) {
    parent
        .spawn((
            Node {
                padding: UiRect::all(px(8)),
                column_gap: px(10),
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(PANEL),
        ))
        .with_children(|row| {
            row.spawn((
                Node {
                    flex_grow: 1.0,
                    ..default()
                },
                text(name, 13., INK),
            ));
            buttons(row);
        });
}

fn title(
    parent: &mut ChildSpawnerCommands,
    eyebrow: &str,
    heading: &str,
    detail: impl Into<String>,
) {
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Column,
            row_gap: px(2),
            ..default()
        })
        .with_children(|title| {
            title.spawn(text(eyebrow, 12., MINT));
            title.spawn(text(heading, 39., INK));
            title.spawn(text(detail, 15., MUTED));
        });
}

fn pager(
    parent: &mut ChildSpawnerCommands,
    page: usize,
    pages: usize,
    previous: Action,
    next: Action,
) {
    parent
        .spawn(Node {
            column_gap: px(12),
            align_items: AlignItems::Center,
            ..default()
        })
        .with_children(|row| {
            if page > 0 {
                button(row, "Previous", previous, false);
            }
            row.spawn(text(format!("Page {} / {pages}", page + 1), 14., MUTED));
            if page + 1 < pages {
                button(row, "Next", next, false);
            }
        });
}

fn button(parent: &mut ChildSpawnerCommands, title: &str, action: Action, active: bool) {
    let idle = if active { ACTIVE } else { TILE };
    parent
        .spawn((
            MenuButton(action),
            HoverSurface {
                idle,
                hover: Color::srgb_u8(67, 101, 113),
            },
            Node {
                padding: UiRect::axes(px(12), px(5)),
                border: UiRect::all(px(1)),
                ..default()
            },
            BackgroundColor(idle),
            BorderColor::all(if active { MINT } else { EDGE }),
        ))
        .observe(click)
        .observe(hover_in)
        .observe(hover_out)
        .with_child((text(title, 13., INK), Pickable::IGNORE));
}

fn verb(parent: &mut ChildSpawnerCommands, title: &str, action: Action) {
    parent
        .spawn((
            MenuButton(action),
            Node {
                min_width: px(174),
                padding: UiRect::axes(px(18), px(6)),
                ..default()
            },
            BackgroundColor(Color::NONE),
            HoverSurface {
                idle: Color::NONE,
                hover: Color::srgba_u8(58, 76, 87, 175),
            },
        ))
        .observe(click)
        .observe(hover_in)
        .observe(hover_out)
        .with_child((text(title, 16., INK), Pickable::IGNORE));
}

fn link(parent: &mut ChildSpawnerCommands, title: &str, action: Action) {
    parent
        .spawn((
            MenuButton(action),
            Node {
                padding: UiRect::all(px(3)),
                ..default()
            },
            BackgroundColor(Color::NONE),
            HoverSurface {
                idle: Color::NONE,
                hover: Color::srgba_u8(58, 76, 87, 140),
            },
        ))
        .observe(click)
        .observe(hover_in)
        .observe(hover_out)
        .with_child((text(title, 12., MINT), Pickable::IGNORE));
}

fn checkbox_link(parent: &mut ChildSpawnerCommands, title: &str, checked: bool, action: Action) {
    parent
        .spawn((
            MenuButton(action),
            Node {
                padding: UiRect::all(px(3)),
                align_items: AlignItems::Center,
                column_gap: px(7),
                ..default()
            },
            BackgroundColor(Color::NONE),
            HoverSurface {
                idle: Color::NONE,
                hover: Color::srgba_u8(58, 76, 87, 140),
            },
        ))
        .observe(click)
        .observe(hover_in)
        .observe(hover_out)
        .with_children(|row| {
            row.spawn((
                Node {
                    width: px(12),
                    height: px(12),
                    padding: UiRect::all(px(2)),
                    border: UiRect::all(px(1)),
                    ..default()
                },
                BorderColor::all(MINT),
                Pickable::IGNORE,
            ))
            .with_child((
                Node {
                    width: percent(100),
                    height: percent(100),
                    ..default()
                },
                BackgroundColor(if checked { MINT } else { Color::NONE }),
                Pickable::IGNORE,
            ));
            row.spawn((text(title, 12., MINT), Pickable::IGNORE));
        });
}

fn hover_in(event: On<PointerOver>, mut buttons: Query<(&HoverSurface, &mut BackgroundColor)>) {
    if let Ok((style, mut background)) = buttons.get_mut(event.entity) {
        background.0 = style.hover;
    }
}

fn hover_out(event: On<PointerOut>, mut buttons: Query<(&HoverSurface, &mut BackgroundColor)>) {
    if let Ok((style, mut background)) = buttons.get_mut(event.entity) {
        background.0 = style.idle;
    }
}

fn click(event: On<PointerClick>, buttons: Query<&MenuButton>, mut menu: ResMut<MenuState>) {
    if event.button == PointerButton::Primary
        && let Ok(button) = buttons.get(event.entity)
    {
        menu.actions.push(button.0.clone());
    }
}

fn text(value: impl Into<String>, size: f32, color: Color) -> (Text, TextFont, TextColor) {
    (
        Text::new(value),
        TextFont {
            font_size: size.into(),
            ..default()
        },
        TextColor(color),
    )
}

fn crosshair_rgb(style: CrosshairStyle) -> String {
    let [r, g, b] = style.rgb;
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn parse_rgb(value: &str) -> Option<[u8; 3]> {
    let value = value.trim();
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some([
        u8::from_str_radix(&value[..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..], 16).ok()?,
    ])
}

fn hud_corner(
    top: Option<f32>,
    left: Option<f32>,
    right: Option<f32>,
    bottom: Option<f32>,
) -> Node {
    Node {
        position_type: PositionType::Absolute,
        top: top.map_or(Val::Auto, px),
        left: left.map_or(Val::Auto, px),
        right: right.map_or(Val::Auto, px),
        bottom: bottom.map_or(Val::Auto, px),
        flex_direction: FlexDirection::Column,
        row_gap: px(4),
        ..default()
    }
}

fn format_time(seconds: f64) -> String {
    let millis = (seconds * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}.{:03}",
        millis / 60_000,
        millis / 1000 % 60,
        millis % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::input::mouse::AccumulatedMouseMotion;
    use bevy_surf::{course::Course, tutorial};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[test]
    fn practice_browses_local_maps_without_catalog_tiles() {
        let data = std::env::temp_dir().join(format!("surfd-practice-menu-{}", std::process::id()));
        let mut app = open_surf_school(&data);
        let mut library = app.world_mut().resource_mut::<MapLibrary>();
        let practice_id = "practice:bhop_flartstrafe_simpsons";
        library.entries.retain(|entry| {
            !entry.is_local() && (!entry.is_practice_download() || entry.id == practice_id)
        });
        let practice_name = library
            .entries
            .iter()
            .find(|entry| entry.id == practice_id)
            .unwrap()
            .name
            .clone();
        let index = library.entries.len();
        library.entries.push(MapEntry {
            id: "local:bhop_easy".into(),
            name: "bhop_easy".into(),
            detail: "Source bhop · Local practice".into(),
            mode: MapMode::Bhop,
            tier: Some(2),
            source: crate::library::MapSource::Source(data.join("bhop_easy.bsp")),
        });

        app.world_mut()
            .resource_mut::<MenuState>()
            .go(Page::Practice);
        app.update();
        choose_menu_action(&mut app, |action| matches!(action, Action::BrowseLocalMaps));
        assert_eq!(app.world().resource::<MenuState>().page, Page::Maps);
        assert!(!has_text(app.world_mut(), "bhop_easy"));
        choose_menu_action(&mut app, |action| {
            matches!(action, Action::MapMode(MapMode::Bhop))
        });
        assert!(has_text(app.world_mut(), "bhop_easy"));
        assert!(has_text(app.world_mut(), &practice_name));
        assert!(has_text(app.world_mut(), "Tier 2"));
        assert!(!has_text(app.world_mut(), "Downline"));
        assert!(
            app.world_mut()
                .query::<&MenuButton>()
                .iter(app.world())
                .any(|button| matches!(button.0, Action::Load(candidate) if candidate == index))
        );

        choose_menu_action(&mut app, |action| {
            matches!(action, Action::Open(Page::Maps))
        });
        assert!(!app.world().resource::<MenuState>().local_maps_only);
        assert!(!has_text(app.world_mut(), &practice_name));
        choose_menu_action(&mut app, |action| {
            matches!(action, Action::MapMode(MapMode::Surf))
        });
        assert!(has_text(app.world_mut(), "Kitsune"));
    }

    #[test]
    fn surf_school_opens_loads_and_advances_through_the_desktop_systems() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data =
            std::env::temp_dir().join(format!("surf-school-menu-{}-{unique}", std::process::id()));
        let mut app = open_surf_school(&data);

        let first = &tutorial::LESSONS[0];
        let second = &tutorial::LESSONS[1];
        assert!(has_text(app.world_mut(), "Learn to surf"));
        assert!(!has_text(app.world_mut(), "Start guided Kitsune"));
        start_from_menu(&mut app, first.id);
        assert_eq!(app.world().resource::<MenuState>().page, Page::Playing);
        assert!(has_text(app.world_mut(), first.goal));

        {
            let mut game = app.world_mut().resource_mut::<Game>();
            game.session.phase = RunPhase::Finished;
            game.session.elapsed = first.gold_seconds;
        }
        app.world_mut().run_schedule(FixedUpdate);
        app.update();
        assert_eq!(app.world().resource::<MenuState>().page, Page::Finished);
        assert!(has_text(app.world_mut(), "Gold medal  /  Personal best"));
        let reopened = MenuState::new(&data);
        assert_eq!(reopened.training.best(first.id), Some(first.gold_seconds));
        assert!(reopened.training.unlocked(second.id));

        start_from_menu(&mut app, second.id);
        assert_eq!(app.world().resource::<MenuState>().page, Page::Playing);
        assert!(has_text(app.world_mut(), second.goal));
        fs::remove_file(data.join("training").join(first.id)).unwrap();
        fs::remove_file(data.join("training").join(".lock")).unwrap();
        fs::remove_dir(data.join("training")).unwrap();
        fs::remove_dir(data).unwrap();
    }

    #[test]
    #[ignore = "requires SURF_GUIDE_KITSUNE_CACHE and SURF_GUIDE_KITSUNE_INPUTS"]
    fn graduated_school_loads_and_coaches_a_real_kitsune_clear() {
        let source_cache = PathBuf::from(std::env::var_os("SURF_GUIDE_KITSUNE_CACHE").unwrap());
        let commands_path = PathBuf::from(std::env::var_os("SURF_GUIDE_KITSUNE_INPUTS").unwrap());
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data =
            std::env::temp_dir().join(format!("surf-starter-{}-{unique}", std::process::id()));
        copy_catalog_cache(&data, "kitsune", &source_cache);
        let training = data.join("training");
        fs::create_dir(&training).unwrap();
        for lesson in tutorial::LESSONS {
            fs::File::create_new(training.join(lesson.id)).unwrap();
        }
        let mut app = open_surf_school(&data);
        app.world_mut().resource_mut::<Online>().endpoint = "http://127.0.0.1:9".into();
        app.world_mut()
            .resource_mut::<MenuState>()
            .settings
            .set_binding(
                ControlAction::RestartStage,
                None,
                Binding::key(KeyCode::KeyX),
            );
        choose_menu_action(&mut app, |action| {
            matches!(
                action,
                Action::Request(MenuRequest::StartPathCourse(crate::library::STARTER_MAP))
            )
        });
        wait_for_catalog_map(&mut app, crate::library::STARTER_MAP);
        let game = app.world().resource::<Game>();
        assert!(game.starter_guide() && game.tutorial_lesson().is_none());
        assert!(game.session.ranked_eligible());
        assert!(has_text(app.world_mut(), "Kitsune · Stage 1/9 · Red"));
        let retry = "X retries a stage during a run and keeps your timer.";
        assert!(has_text(app.world_mut(), retry));
        app.world_mut().resource_mut::<Game>().set_practice(Some(1));
        app.update();
        assert!(has_text(app.world_mut(), "Kitsune · Stage practice"));
        assert!(!has_text(app.world_mut(), retry));
        assert!(!app.world().resource::<Game>().session.ranked_eligible());
        app.world_mut().resource_mut::<Game>().set_practice(None);

        // Walk off the deck without releasing forward to trigger the live correction.
        for _ in 0..300 {
            app.world_mut().resource_mut::<Game>().input.forward = 1.0;
            app.world_mut().run_schedule(FixedUpdate);
            let run = &app.world().resource::<Game>().session;
            if run.phase == RunPhase::Running && !run.player.grounded {
                break;
            }
        }
        app.update();
        assert!(has_text(
            app.world_mut(),
            "Release W and steer toward the ridge with A or D."
        ));
        app.world_mut().resource_mut::<Game>().restart();

        let elapsed =
            clear_original_catalog_course(&mut app, &commands_path, 91_940_469, 8, |app| {
                assert!(has_text(app.world_mut(), "Kitsune · Stage 2/9 · Orange"));
            });
        assert!((elapsed - 91.940_469_122_672).abs() < 1e-8);
        assert!(has_text(app.world_mut(), "Course path · 1/3 cleared"));
        assert!(has_text(app.world_mut(), "Play Tendies"));
        let saved = MenuState::new(&data);
        assert_eq!(saved.records["surf_kitsune"].seconds, elapsed);
        assert!(saved.training.next().is_none());

        choose_menu_action(&mut app, |action| {
            matches!(action, Action::Open(Page::Pause))
        });
        choose_menu_action(&mut app, |action| {
            matches!(action, Action::HideStarterGuide)
        });
        assert!(!app.world().resource::<Game>().starter_guide());
        assert_eq!(app.world().resource::<Game>().session.elapsed, elapsed);
        if let Some(inputs) = std::env::var_os("SURF_PATH_INPUTS_DIR") {
            finish_course_path(
                &mut app,
                &data,
                source_cache.parent().unwrap(),
                Path::new(&inputs),
            );
        }
        fs::remove_dir_all(data).unwrap();
    }

    fn finish_course_path(app: &mut App, data: &Path, cache: &Path, inputs: &Path) {
        let rookie_cache = PathBuf::from(std::env::var_os("SURF_PATH_ROOKIE_CACHE").unwrap());
        copy_catalog_cache(data, "tendies", &cache.join("tendies"));
        copy_catalog_cache(data, "rookie", &rookie_cache);

        app.world_mut()
            .resource_mut::<MenuState>()
            .go(Page::Finished);
        app.update();
        choose_menu_action(app, |action| {
            matches!(
                action,
                Action::Request(MenuRequest::StartPathCourse("surf_tendies"))
            )
        });
        wait_for_catalog_map(app, "surf_tendies");
        assert!(!app.world().resource::<Game>().starter_guide());
        clear_original_catalog_course(
            app,
            &inputs.join("tendies-complete.json"),
            41_185_245,
            4,
            |_| {},
        );
        assert!(has_text(app.world_mut(), "Course path · 2/3 cleared"));
        assert!(has_text(app.world_mut(), "Play Rookie"));

        // Open the ordinary Maps page and choose Rookie's tile rather than the path button.
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::F2);
        app.update();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset_all();
        assert_eq!(app.world().resource::<MenuState>().page, Page::Maps);
        assert!(has_text(app.world_mut(), "Play Rookie"));
        let index = app
            .world()
            .resource::<MapLibrary>()
            .entries
            .iter()
            .position(|entry| entry.id == "surf_rookie")
            .unwrap();
        let mut menu = app.world_mut().resource_mut::<MenuState>();
        menu.query = "surf_rookie".into();
        menu.map_page = 0;
        menu.refresh();
        app.update();
        choose_menu_action(
            app,
            |action| matches!(action, Action::Load(i) if *i == index),
        );
        wait_for_catalog_map(app, "surf_rookie");
        clear_original_catalog_course(
            app,
            &inputs.join("rookie-complete.json"),
            150_502_324,
            17,
            |_| {},
        );
        assert!(has_text(app.world_mut(), "Course path complete"));

        let saved = MenuState::new(data);
        assert_eq!(saved.records.len(), 3);
        assert!(menu_training::next_path_course(&saved).is_none());
        assert_eq!(menu_training::xp(&saved), 1300);
        let mut reopened = open_surf_school(data);
        assert!(has_text(reopened.world_mut(), "Course path complete"));
        assert!(has_text(reopened.world_mut(), "Replay guided Kitsune"));
        assert!(!has_text(reopened.world_mut(), "Play Rookie"));
    }

    fn copy_catalog_cache(data: &Path, slug: &str, source: &Path) {
        let cache = data.join(".surfd-cache").join(slug);
        fs::create_dir_all(&cache).unwrap();
        for asset in fs::read_dir(source).unwrap() {
            let asset = asset.unwrap();
            if asset.file_type().unwrap().is_file() {
                fs::copy(asset.path(), cache.join(asset.file_name())).unwrap();
            }
        }
    }

    fn wait_for_catalog_map(app: &mut App, id: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while app.world().resource::<Game>().pending.is_some() {
            app.update();
            assert!(Instant::now() < deadline, "{id} loader did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
        let game = app.world().resource::<Game>();
        assert_eq!(
            game.course.name,
            id,
            "{}",
            app.world().resource::<MenuState>().notice
        );
        assert!(!game.course.visual_meshes.is_empty());
        assert_eq!(app.world().resource::<MenuState>().page, Page::Playing);
    }

    fn clear_original_catalog_course(
        app: &mut App,
        path: &Path,
        finish_us: u64,
        split_count: usize,
        on_first_split: impl FnOnce(&mut App),
    ) -> f64 {
        use bevy_surf::{
            movement::{MoveInput, source_yaw_to_native},
            replay::{TickCommand, verify_ranked},
        };

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct OriginalInputs {
            map_id: String,
            commands: Vec<OriginalCommand>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct OriginalCommand {
            forward: f32,
            side: f32,
            yaw: f64,
            pitch: f64,
            jump: bool,
            duck: bool,
            #[serde(default)]
            restart_stage: bool,
        }

        let original: OriginalInputs = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(app.world().resource::<Game>().course.name, original.map_id);
        let mut first_split = Some(on_first_split);
        for command in original.commands {
            let mut game = app.world_mut().resource_mut::<Game>();
            let game = &mut *game;
            game.input = MoveInput {
                forward: command.forward / 400.0,
                strafe: command.side / 400.0,
                yaw: source_yaw_to_native(command.yaw),
                jump: command.jump,
                crouch: command.duck,
            };
            game.session.tick_command(
                &game.course,
                TickCommand {
                    input: game.input,
                    pitch: -command.pitch.to_radians() as f32,
                    restart_stage: command.restart_stage,
                    source_yaw_degrees: Some(command.yaw),
                },
            );
            if game.session.phase == RunPhase::Finished {
                break;
            }
            if game.session.splits.len() == 1
                && let Some(on_first_split) = first_split.take()
            {
                app.update();
                on_first_split(app);
            }
        }
        let mut game = app.world_mut().resource_mut::<Game>();
        let game = &mut *game;
        assert!(first_split.is_none() && game.session.ranked_eligible());
        assert_eq!(game.session.phase, RunPhase::Finished);
        let elapsed = game.session.elapsed;
        let replay = game.session.take_finished_replay(&game.course).unwrap();
        let verified = verify_ranked(&game.course, &replay).unwrap();
        assert_eq!(verified.time_us, finish_us);
        assert_eq!(verified.splits_us.len(), split_count);
        app.update();
        assert_eq!(app.world().resource::<MenuState>().page, Page::Finished);
        assert!(has_text(app.world_mut(), "+200 XP"));
        elapsed
    }

    #[test]
    fn surf_school_demo_shows_real_inputs_and_returns_to_the_live_section_without_rewards() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data =
            std::env::temp_dir().join(format!("surf-school-demo-{}-{unique}", std::process::id()));
        let mut app = open_surf_school(&data);
        let first = &tutorial::LESSONS[0];
        let second = &tutorial::LESSONS[1];
        let world = app.world_mut();
        let mut menu = world.resource_mut::<MenuState>();
        menu.settings.show_keys = false;
        menu.actions
            .push(Action::Request(MenuRequest::WatchLesson(second.id)));
        app.update();
        assert_eq!(app.world().resource::<MenuState>().page, Page::Training);

        choose_menu_action(
            &mut app,
            |action| matches!(action, Action::Request(MenuRequest::WatchLesson(id)) if *id == first.id),
        );
        assert_eq!(app.world().resource::<MenuState>().page, Page::Playing);
        assert!(has_text(app.world_mut(), "LESSON DEMO"));
        assert!(has_text(app.world_mut(), "Play lesson"));
        let mut saw_jump = false;
        for _ in 0..660 {
            app.world_mut().run_schedule(FixedUpdate);
            let game = app.world().resource::<Game>();
            saw_jump |= game.displayed_input().jump;
            if game.displayed_session().phase == RunPhase::Finished {
                break;
            }
        }
        let game = app.world().resource::<Game>();
        assert!(saw_jump && game.watching_lesson_demo());
        assert_eq!(game.displayed_session().phase, RunPhase::Finished);
        assert!(game.displayed_session().elapsed <= first.gold_seconds);
        assert_eq!(game.session.phase, RunPhase::Ready);
        assert!(!game.has_replay());
        app.update();
        assert!(TrainingProgress::read(&data).best(first.id).is_none());
        assert!(!TrainingProgress::read(&data).unlocked(second.id));

        choose_menu_action(&mut app, |action| {
            matches!(action, Action::ReturnFromLessonDemo)
        });
        assert!(has_text(app.world_mut(), "SURF SCHOOL"));
        assert!(!has_text(app.world_mut(), "Play lesson"));

        for _ in 0..240 {
            app.world_mut().resource_mut::<Game>().input.forward = 1.0;
            app.world_mut().run_schedule(FixedUpdate);
            if app.world().resource::<Game>().session.splits.len() == 1 {
                break;
            }
        }
        let game = app.world().resource::<Game>();
        assert_eq!(game.session.phase, RunPhase::Running);
        assert_eq!(game.session.splits.len(), 1);
        let expected = (
            game.session.player.position,
            game.session.player.velocity,
            game.session.elapsed,
            game.session.splits.clone(),
            game.look_angles(),
            game.run_epoch,
        );
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Escape);
        app.update();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset_all();
        assert_eq!(app.world().resource::<MenuState>().page, Page::Pause);
        choose_menu_action(
            &mut app,
            |action| matches!(action, Action::Request(MenuRequest::WatchLesson(id)) if *id == first.id),
        );
        app.world_mut().run_schedule(FixedUpdate);
        choose_menu_action(&mut app, |action| {
            matches!(action, Action::ReturnFromLessonDemo)
        });
        let game = app.world().resource::<Game>();
        assert!(!game.watching_replay());
        assert_eq!(
            (
                game.session.player.position,
                game.session.player.velocity,
                game.session.elapsed,
                game.session.splits.clone(),
                game.look_angles(),
                game.run_epoch,
            ),
            expected,
        );
        assert!(has_text(app.world_mut(), "Corner and jump"));
        if data.exists() {
            fs::remove_dir(data).unwrap();
        }
    }

    #[test]
    fn surf_school_retries_compare_a_saved_clear_at_the_live_checkpoint() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data =
            std::env::temp_dir().join(format!("surf-school-coach-{}-{unique}", std::process::id()));
        let training = data.join("training");
        fs::create_dir_all(&training).unwrap();
        for lesson in tutorial::LESSONS {
            fs::File::create_new(training.join(lesson.id)).unwrap();
        }
        let bhop = "tutorial-bhop";
        fs::write(training.join(bhop), "3.0").unwrap();
        let mut app = open_surf_school(&data);
        {
            let mut menu = app.world_mut().resource_mut::<MenuState>();
            assert_eq!(menu.training.best(bhop), Some(3.0));
            assert!(menu.training.last_clear(bhop).is_none());
            menu.settings.compare_speed = true;
            menu.settings
                .set_binding(ControlAction::Jump, Some(0), Binding::key(KeyCode::KeyX));
        }
        start_from_menu(&mut app, bhop);
        let first = clear_bhop_with_live_coach(
            &mut app,
            -196.0,
            "First jump · 00:00.720\n260 units/s",
            None,
        );

        // Reopen the saved progress before taking the finish card's retry action.
        let saved = TrainingProgress::read(&data);
        assert_eq!(saved.best(bhop), Some(first));
        assert!(saved.last_clear(bhop).is_some());
        let world = app.world_mut();
        assert!(
            world
                .query::<&MenuButton>()
                .iter(world)
                .any(|button| matches!(button.0, Action::Restart))
        );
        let mut menu = world.resource_mut::<MenuState>();
        menu.training = saved;
        menu.actions.push(Action::Restart);
        app.update();
        assert_eq!(app.world().resource::<MenuState>().page, Page::Playing);
        assert!(!has_text(
            app.world_mut(),
            "First jump · 00:00.720\n260 units/s"
        ));
        let second = clear_bhop_with_live_coach(
            &mut app,
            -246.0,
            "First jump · 00:00.915 (+0.195s vs last clear)\n260 units/s",
            Some("00:00.740"),
        );
        assert!((second - first).abs() < 1e-9);
        assert_eq!(
            TrainingProgress::read(&data).best(bhop),
            Some(first.min(second))
        );
        check_bhop_section_drill(&mut app, &training.join(bhop), second);

        for lesson in tutorial::LESSONS {
            fs::remove_file(training.join(lesson.id)).unwrap();
        }
        fs::remove_file(training.join(".lock")).unwrap();
        fs::remove_dir(training).unwrap();
        fs::remove_dir(data).unwrap();
    }

    #[test]
    fn climb_coach_corrects_early_release_and_restores_the_paused_approach() {
        use crate::view::{DrillResult, LiveLessonCue};
        use bevy_surf::movement::MoveInput;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data =
            std::env::temp_dir().join(format!("surf-kicker-{}-{unique}", std::process::id()));
        let training = data.join("training");
        fs::create_dir_all(&training).unwrap();
        let id = "tutorial-rise";
        for lesson in tutorial::LESSONS
            .iter()
            .take_while(|lesson| lesson.id != id)
        {
            fs::File::create_new(training.join(lesson.id)).unwrap();
        }
        let mut app = open_surf_school(&data);
        app.world_mut()
            .resource_mut::<MenuState>()
            .settings
            .set_binding(ControlAction::Left, Some(0), Binding::key(KeyCode::KeyQ));
        start_from_menu(&mut app, id);

        let tick = |app: &mut App, correct: bool| {
            let mut game = app.world_mut().resource_mut::<Game>();
            let p = game.session.player.position;
            let v = game.session.player.velocity;
            let forward = p.z > -250.0 || (p.z < -1500.0 && game.session.player.grounded);
            let steer = p.z > -1400.0 && (p.z > -880.0 || correct) && (p.x > 76.0 || v.x > 22.0);
            game.input = MoveInput {
                forward: if forward { 1.0 } else { 0.0 },
                strafe: if !forward && steer { -1.0 } else { 0.0 },
                ..Default::default()
            };
            app.world_mut().run_schedule(FixedUpdate);
        };
        let prompt = "Tap Q toward the upper stripe. Stay high through the violet bend.";
        let reach_cue = |app: &mut App| {
            for _ in 0..300 {
                tick(app, false);
                let game = app.world().resource::<Game>();
                if game.live_lesson_cue() == Some(LiveLessonCue::StayHigh) {
                    assert!(game.session.player.position.z > -1210.0);
                    app.update();
                    assert!(has_text(app.world_mut(), prompt));
                    return;
                }
            }
            panic!("released inward strafe should prompt before the violet bend");
        };
        reach_cue(&mut app);
        for _ in 0..200 {
            tick(&mut app, false);
            if app.world().resource::<Game>().session.phase == RunPhase::Ready {
                break;
            }
        }
        assert_eq!(
            app.world().resource::<Game>().session.phase,
            RunPhase::Ready
        );
        assert!(!training.join(id).exists());
        app.world_mut().resource_mut::<Game>().restart();
        reach_cue(&mut app);
        let game = app.world().resource::<Game>();
        let original = (
            game.run_epoch,
            game.session.elapsed,
            game.session.player.position,
            game.session.player.velocity,
        );

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Escape);
        app.update();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset_all();
        choose_menu_action(&mut app, |action| {
            matches!(action, Action::PracticeLessonSection(0))
        });
        choose_menu_action(&mut app, |action| matches!(action, Action::Restart));
        assert!(!has_text(app.world_mut(), prompt));
        reach_cue(&mut app);
        for _ in 0..200 {
            tick(&mut app, true);
            if app.world().resource::<Game>().drill_result().is_some() {
                break;
            }
        }
        app.update();
        assert!(matches!(
            app.world().resource::<Game>().drill_result(),
            Some(DrillResult::Cleared(_))
        ));
        assert!(!training.join(id).exists());
        choose_menu_action(&mut app, |action| matches!(action, Action::ExitLessonDrill));
        let game = app.world().resource::<Game>();
        assert_eq!(
            (
                game.run_epoch,
                game.session.elapsed,
                game.session.player.position,
                game.session.player.velocity
            ),
            original
        );
        assert_eq!(game.live_lesson_cue(), Some(LiveLessonCue::StayHigh));

        choose_menu_action(&mut app, |action| matches!(action, Action::Resume));
        for _ in 0..12 {
            tick(&mut app, false);
        }
        for _ in 0..300 {
            tick(&mut app, true);
            assert_eq!(app.world().resource::<Game>().live_lesson_cue(), None);
            if app.world().resource::<Game>().session.phase == RunPhase::Finished {
                break;
            }
        }
        app.update();
        assert!(has_text(app.world_mut(), "Lesson complete"));
        assert!(TrainingProgress::read(&data).best(id).is_some());
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn bunnyhop_coach_warns_before_both_pad_landings_and_recovers_the_single_jump() {
        use crate::view::LiveLessonCue;
        use bevy_surf::movement::MoveInput;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data =
            std::env::temp_dir().join(format!("surf-pad-coach-{}-{unique}", std::process::id()));
        let training = data.join("training");
        fs::create_dir_all(&training).unwrap();
        let id = "tutorial-bhop";
        for lesson in tutorial::LESSONS
            .iter()
            .take_while(|lesson| lesson.id != id)
        {
            fs::File::create_new(training.join(lesson.id)).unwrap();
        }
        let mut app = open_surf_school(&data);
        {
            let mut menu = app.world_mut().resource_mut::<MenuState>();
            menu.settings
                .set_binding(ControlAction::Jump, Some(0), Binding::WheelUp);
            menu.settings
                .set_binding(ControlAction::Jump, Some(1), Binding::key(KeyCode::KeyX));
        }
        start_from_menu(&mut app, id);
        let prompt = "Hold X as you land on the violet pad to hop immediately.";
        let mut first_prompt = None;
        for recover in [false, true] {
            app.world_mut().resource_mut::<Game>().restart();
            let mut launched = false;
            let mut next_hop = None;
            let mut lead_ticks = Vec::new();
            for frame in 0..500 {
                let game = app.world().resource::<Game>();
                let grounded = game.session.player.grounded;
                let jump = if !launched && game.session.player.position.z <= -220.0 {
                    launched = true;
                    true
                } else {
                    recover && next_hop.is_some_and(|cue| frame >= cue + 12)
                };
                app.world_mut().resource_mut::<Game>().input = MoveInput {
                    forward: 1.0,
                    jump,
                    ..Default::default()
                };
                app.world_mut().run_schedule(FixedUpdate);
                let game = app.world().resource::<Game>();
                if game.session.phase == RunPhase::Finished
                    || (launched && game.session.phase == RunPhase::Ready)
                {
                    break;
                }
                if jump {
                    assert_eq!(game.live_lesson_cue(), None);
                    if grounded
                        && !game.session.player.grounded
                        && game.session.player.velocity.y > 0.0
                    {
                        next_hop = None;
                    }
                }
                if let Some(cue) = next_hop
                    && !grounded
                    && game.session.player.grounded
                {
                    lead_ticks.push(frame - cue);
                }
                if next_hop.is_none() && game.live_lesson_cue() == Some(LiveLessonCue::HopOnLanding)
                {
                    assert!(!game.session.player.grounded);
                    if lead_ticks.is_empty() {
                        let state = (
                            frame,
                            game.session.player.position,
                            game.session.player.velocity,
                        );
                        if let Some(first) = first_prompt {
                            assert_eq!(state, first);
                        } else {
                            first_prompt = Some(state);
                        }
                    }
                    next_hop = Some(frame);
                    app.update();
                    assert!(has_text(app.world_mut(), prompt));
                }
            }
            let game = app.world().resource::<Game>();
            assert_eq!(lead_ticks.len(), if recover { 2 } else { 1 });
            assert!(lead_ticks.into_iter().all(|ticks| ticks >= 12));
            assert_eq!(
                game.session.phase,
                if recover {
                    RunPhase::Finished
                } else {
                    RunPhase::Ready
                }
            );
            app.update();
            if !recover {
                assert!(!training.join(id).exists());
            }
        }
        assert!(has_text(app.world_mut(), "Lesson complete"));
        assert!(TrainingProgress::read(&data).best(id).is_some());
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn face_transfer_coach_recovers_the_missed_switch_and_returns_to_the_paused_lesson() {
        use crate::view::{DrillResult, LiveLessonCue};
        use bevy_surf::movement::MoveInput;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data = std::env::temp_dir().join(format!(
            "surf-face-transfer-{}-{unique}",
            std::process::id()
        ));
        let training = data.join("training");
        fs::create_dir_all(&training).unwrap();
        let id = "tutorial-transfer";
        for lesson in tutorial::LESSONS
            .iter()
            .take_while(|lesson| lesson.id != id)
        {
            fs::File::create_new(training.join(lesson.id)).unwrap();
        }
        let mut app = open_surf_school(&data);
        {
            let mut menu = app.world_mut().resource_mut::<MenuState>();
            menu.settings
                .set_binding(ControlAction::Left, Some(0), Binding::key(KeyCode::KeyQ));
            menu.settings
                .set_binding(ControlAction::Right, Some(0), Binding::key(KeyCode::KeyX));
        }
        start_from_menu(&mut app, id);
        let tick = |app: &mut App, input| {
            app.world_mut().resource_mut::<Game>().input = input;
            app.world_mut().run_schedule(FixedUpdate);
        };
        let demo = tutorial::demonstration(&app.world().resource::<Game>().course).unwrap();
        for command in demo.commands {
            tick(&mut app, command.input);
            if app.world().resource::<Game>().session.splits.len() == 1 {
                break;
            }
        }
        let game = app.world().resource::<Game>();
        assert_eq!(game.session.splits.len(), 1);
        let original = (
            game.run_epoch,
            game.session.elapsed,
            game.session.player.position,
            game.session.player.velocity,
            game.look_angles(),
            game.session.splits.clone(),
        );
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Escape);
        app.update();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset_all();
        choose_menu_action(&mut app, |action| {
            matches!(action, Action::PracticeLessonSection(1))
        });

        let prompt = "Release Q. Steer toward the violet ramp with X.";
        let mut first_cue = None;
        for recover in [false, true] {
            choose_menu_action(&mut app, |action| matches!(action, Action::Restart));
            assert!(!has_text(app.world_mut(), prompt));
            let mut saw_cue = false;
            for _ in 0..280 {
                let game = app.world().resource::<Game>();
                let p = game.session.player.position;
                let v = game.session.player.velocity;
                let strafe = if p.z > -1165.0 {
                    if p.x > 116.0 || v.x > 26.0 { -1.0 } else { 0.0 }
                } else if !recover || !saw_cue {
                    -1.0
                } else if p.z > -1495.0 || (p.z > -2480.0 && (p.x < 235.0 || v.x < -25.0)) {
                    1.0
                } else {
                    0.0
                };
                tick(
                    &mut app,
                    MoveInput {
                        strafe,
                        ..Default::default()
                    },
                );
                let game = app.world().resource::<Game>();
                let cue = game.live_lesson_cue();
                if p.z > -1240.0 || (recover && saw_cue) {
                    assert_eq!(cue, None);
                }
                if !saw_cue && cue == Some(LiveLessonCue::SwitchRight) {
                    assert!(p.z < -1240.0 && p.z > -1510.0);
                    let pose = (
                        game.session.player.position,
                        game.session.player.velocity,
                        game.session.elapsed,
                    );
                    if let Some(first) = first_cue {
                        assert_eq!(pose, first);
                    } else {
                        first_cue = Some(pose);
                    }
                    saw_cue = true;
                    app.update();
                    assert!(has_text(app.world_mut(), prompt));
                }
                if app.world().resource::<Game>().drill_result().is_some() {
                    break;
                }
            }
            app.update();
            assert!(saw_cue);
            let result = app.world().resource::<Game>().drill_result();
            if recover {
                assert!(matches!(result, Some(DrillResult::Cleared(_))));
                assert!(has_text(app.world_mut(), "Section cleared"));
            } else {
                assert!(matches!(result, Some(DrillResult::Missed(_))));
            }
        }
        assert!(!training.join(id).exists());
        choose_menu_action(&mut app, |action| matches!(action, Action::ExitLessonDrill));
        assert_eq!(app.world().resource::<MenuState>().page, Page::Pause);
        let game = app.world().resource::<Game>();
        assert_eq!(
            (
                game.run_epoch,
                game.session.elapsed,
                game.session.player.position,
                game.session.player.velocity,
                game.look_angles(),
                game.session.splits.clone()
            ),
            original
        );
        assert_eq!(game.live_lesson_cue(), None);
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn ramp_entry_coach_catches_forward_and_outward_inputs_before_the_miss() {
        use crate::view::{DrillResult, LiveLessonCue};
        use bevy_surf::movement::MoveInput;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data =
            std::env::temp_dir().join(format!("surf-ramp-entry-{}-{unique}", std::process::id()));
        let training = data.join("training");
        fs::create_dir_all(&training).unwrap();
        fs::File::create_new(training.join("tutorial-movement")).unwrap();
        let lesson = "tutorial-catch";
        let mut app = open_surf_school(&data);
        {
            let mut menu = app.world_mut().resource_mut::<MenuState>();
            menu.settings
                .set_binding(ControlAction::Forward, Some(0), Binding::key(KeyCode::KeyX));
            menu.settings
                .set_binding(ControlAction::Left, Some(0), Binding::key(KeyCode::KeyQ));
        }
        start_from_menu(&mut app, lesson);

        let tick = |app: &mut App, input: MoveInput| {
            app.world_mut().resource_mut::<Game>().input = input;
            app.world_mut().run_schedule(FixedUpdate);
        };
        let forward = MoveInput {
            forward: 1.0,
            ..Default::default()
        };
        let right = MoveInput {
            strafe: 1.0,
            ..Default::default()
        };
        let left = MoveInput {
            strafe: -1.0,
            ..Default::default()
        };
        let hold_until = |app: &mut App, input, expected| {
            for _ in 0..180 {
                tick(app, input);
                if app.world().resource::<Game>().live_lesson_cue() == Some(expected) {
                    app.update();
                    return;
                }
            }
            panic!("the ramp approach should show {expected:?}");
        };
        let release = "Release X now. Let the drop carry you onto the ramp.";
        let steer = "Tap Q toward the ridge. You’re drifting right.";
        for _ in 0..10 {
            tick(&mut app, forward);
        }
        app.update();
        assert!(!has_text(app.world_mut(), release));
        hold_until(&mut app, forward, LiveLessonCue::ReleaseForward);
        assert!(has_text(app.world_mut(), release));
        let game = app.world().resource::<Game>();
        let original = (
            game.run_epoch,
            game.session.elapsed,
            game.session.player.position,
            game.session.player.velocity,
        );

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Escape);
        app.update();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset_all();
        choose_menu_action(&mut app, |action| {
            matches!(action, Action::PracticeLessonSection(0))
        });
        choose_menu_action(&mut app, |action| matches!(action, Action::Restart));
        assert!(!has_text(app.world_mut(), release));
        hold_until(&mut app, forward, LiveLessonCue::ReleaseForward);
        assert!(has_text(app.world_mut(), release));
        hold_until(&mut app, right, LiveLessonCue::SteerLeft);
        assert!(has_text(app.world_mut(), steer));
        let outward = app.world().resource::<Game>().session.player.velocity.x;
        tick(&mut app, left);
        app.update();
        assert!(!has_text(app.world_mut(), steer));
        assert!(!has_text(app.world_mut(), release));
        assert!(app.world().resource::<Game>().session.player.velocity.x < outward);

        let ride = |game: &Game| {
            let p = game.session.player.position;
            let v = game.session.player.velocity;
            MoveInput {
                forward: if p.z < -1470.0 && game.session.player.grounded {
                    1.0
                } else {
                    0.0
                },
                strafe: if p.z > -1410.0 && (p.x > 120.0 || v.x > 28.0) {
                    -1.0
                } else {
                    0.0
                },
                ..Default::default()
            }
        };
        for _ in 0..300 {
            let input = ride(app.world().resource::<Game>());
            tick(&mut app, input);
            if app.world().resource::<Game>().drill_result().is_some() {
                break;
            }
        }
        app.update();
        assert!(matches!(
            app.world().resource::<Game>().drill_result(),
            Some(DrillResult::Cleared(_))
        ));
        assert!(!training.join(lesson).exists());
        choose_menu_action(&mut app, |action| matches!(action, Action::ExitLessonDrill));
        let game = app.world().resource::<Game>();
        assert_eq!(
            (
                game.run_epoch,
                game.session.elapsed,
                game.session.player.position,
                game.session.player.velocity
            ),
            original
        );
        assert_eq!(game.live_lesson_cue(), Some(LiveLessonCue::ReleaseForward));

        choose_menu_action(&mut app, |action| matches!(action, Action::Resume));
        for _ in 0..400 {
            let input = ride(app.world().resource::<Game>());
            tick(&mut app, input);
            if app.world().resource::<Game>().session.phase == RunPhase::Finished {
                break;
            }
        }
        app.update();
        assert!(has_text(app.world_mut(), "Lesson complete"));
        assert!(TrainingProgress::read(&data).best(lesson).is_some());
        fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn paused_lesson_can_drill_before_its_first_clear_and_resume_the_same_run() {
        use crate::view::{DrillResult, LiveLessonCue};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data = std::env::temp_dir().join(format!(
            "surf-school-first-drill-{}-{unique}",
            std::process::id()
        ));
        let training = data.join("training");
        fs::create_dir_all(&training).unwrap();
        let bhop = "tutorial-bhop";
        for lesson in tutorial::LESSONS
            .iter()
            .take_while(|lesson| lesson.id != bhop)
        {
            fs::File::create_new(training.join(lesson.id)).unwrap();
        }
        let mut app = open_surf_school(&data);
        start_from_menu(&mut app, bhop);
        let demo = tutorial::demonstration(&app.world().resource::<Game>().course).unwrap();
        let entry_tick = demo
            .commands
            .iter()
            .position(|command| {
                app.world_mut().resource_mut::<Game>().input = command.input;
                app.world_mut().run_schedule(FixedUpdate);
                app.world().resource::<Game>().session.splits.len() == 1
            })
            .unwrap();
        let paused_tick = entry_tick + 3;
        for command in &demo.commands[entry_tick + 1..=paused_tick] {
            app.world_mut().resource_mut::<Game>().input = command.input;
            app.world_mut().run_schedule(FixedUpdate);
        }
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Escape);
        app.update();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset_all();
        assert_eq!(app.world().resource::<MenuState>().page, Page::Pause);
        assert!(has_text(app.world_mut(), "Practice middle hop"));
        app.world_mut().resource_mut::<Game>().save_position();
        let game = app.world().resource::<Game>();
        assert_eq!(game.session.phase, RunPhase::Running);
        assert!(game.session.has_saved_position());
        let original_cue = game.live_lesson_cue();
        let original = (
            game.run_epoch,
            game.session.elapsed,
            game.session.player.position,
            game.session.player.velocity,
            game.look_angles(),
            game.session.splits.clone(),
        );
        let xp = menu_training::xp(app.world().resource::<MenuState>());

        choose_menu_action(&mut app, |action| {
            matches!(action, Action::PracticeLessonSection(1))
        });
        assert!(has_text(app.world_mut(), "Return to paused lesson"));
        app.world_mut().resource_mut::<MenuState>().actions.extend([
            Action::Open(Page::Practice),
            Action::Practice(None),
            Action::SavePosition,
            Action::RestorePosition,
            Action::Bonus(None),
        ]);
        app.update();
        assert_eq!(app.world().resource::<MenuState>().page, Page::Finished);
        let game = app.world().resource::<Game>();
        assert_eq!(game.drill_result(), Some(DrillResult::Ready));
        assert!(!game.session.has_saved_position());
        assert!(!game.session.ranked_eligible());
        app.world_mut()
            .resource_mut::<MenuState>()
            .settings
            .clear_binding(ControlAction::Jump, 0);
        choose_menu_action(&mut app, |action| matches!(action, Action::Restart));
        let mut saw_wheel_cue = false;
        let mut tapped = false;
        for command in &demo.commands[entry_tick + 1..] {
            let mut game = app.world_mut().resource_mut::<Game>();
            let jump = !tapped && game.session.player.grounded;
            tapped |= jump;
            game.input = bevy_surf::movement::MoveInput {
                jump,
                ..command.input
            };
            app.world_mut().run_schedule(FixedUpdate);
            if !saw_wheel_cue
                && app.world().resource::<Game>().live_lesson_cue()
                    == Some(LiveLessonCue::HopOnLanding)
            {
                saw_wheel_cue = true;
                app.update();
                assert!(has_text(
                    app.world_mut(),
                    "Scroll down as you touch the violet pad to hop again."
                ));
            }
            if app.world().resource::<Game>().drill_result().is_some() {
                break;
            }
        }
        assert!(tapped && saw_wheel_cue);
        app.update();
        assert!(has_text(app.world_mut(), "Section cleared"));
        assert!(!training.join(bhop).exists());
        assert_eq!(menu_training::xp(app.world().resource::<MenuState>()), xp);

        choose_menu_action(&mut app, |action| matches!(action, Action::ExitLessonDrill));
        assert_eq!(app.world().resource::<MenuState>().page, Page::Pause);
        let game = app.world().resource::<Game>();
        assert!(game.drill_section().is_none());
        assert_eq!(game.session.phase, RunPhase::Running);
        assert!(game.session.has_saved_position());
        assert_eq!(game.live_lesson_cue(), original_cue);
        assert_eq!(
            (
                game.run_epoch,
                game.session.elapsed,
                game.session.player.position,
                game.session.player.velocity,
                game.look_angles(),
                game.session.splits.clone()
            ),
            original
        );
        choose_menu_action(&mut app, |action| matches!(action, Action::Resume));
        for command in &demo.commands[paused_tick + 1..] {
            app.world_mut().resource_mut::<Game>().input = command.input;
            app.world_mut().run_schedule(FixedUpdate);
        }
        app.update();
        assert!(has_text(app.world_mut(), "Lesson complete"));
        assert!(TrainingProgress::read(&data).best(bhop).is_some());
        assert!(menu_training::xp(app.world().resource::<MenuState>()) > xp);
        fs::remove_dir_all(data).unwrap();
    }

    fn check_bhop_section_drill(app: &mut App, progress: &Path, full_clear: f64) {
        use crate::view::{DrillFinding, DrillResult};

        let saved = fs::read(progress).unwrap();
        let xp = menu_training::xp(app.world().resource::<MenuState>());
        let game = app.world().resource::<Game>();
        let demo = tutorial::demonstration(&game.course).unwrap();
        let mut reference = bevy_surf::session::Session::new(&game.course);
        let entry_tick = demo
            .commands
            .iter()
            .position(|&command| {
                reference.tick_command(&game.course, command);
                reference.splits.len() == 1
            })
            .unwrap();
        let entry = (
            reference.player.position,
            reference.player.velocity,
            reference.elapsed,
        );
        assert!(entry.1.length() > 200.0);

        choose_menu_action(app, |action| {
            matches!(action, Action::PracticeLessonSection(1))
        });
        assert!(has_text(app.world_mut(), "Middle hop"));
        assert!(has_text(app.world_mut(), "Start practice"));
        assert!(has_text(
            app.world_mut(),
            "Keep W held and jump with X as you touch the violet pad."
        ));
        app.world_mut().run_schedule(FixedUpdate);
        let game = app.world().resource::<Game>();
        assert_eq!(game.drill_result(), Some(DrillResult::Ready));
        assert_eq!(
            (
                game.session.player.position,
                game.session.player.velocity,
                game.session.elapsed
            ),
            entry
        );
        assert_eq!(game.displayed_elapsed(), 0.0);

        choose_menu_action(app, |action| matches!(action, Action::Restart));
        assert!(has_text(app.world_mut(), "Section practice · Middle hop"));
        for _ in 0..400 {
            app.world_mut().resource_mut::<Game>().input.forward = 1.0;
            app.world_mut().run_schedule(FixedUpdate);
            if app.world().resource::<Game>().drill_result().is_some() {
                break;
            }
        }
        app.update();
        let Some(DrillResult::Missed(feedback)) = app.world().resource::<Game>().drill_result()
        else {
            panic!("walking off the pad should miss this hop");
        };
        let Some(DrillFinding::PadHop {
            milliseconds,
            jumped: false,
        }) = feedback.finding
        else {
            panic!("the missed hop should report the observed ground time");
        };
        assert!(milliseconds >= 45);
        assert!(has_text(
            app.world_mut(),
            &format!("You spent {milliseconds} ms on the violet pad without jumping.")
        ));
        assert!(has_text(
            app.world_mut(),
            "Hold X as you land on the violet pad to hop immediately."
        ));
        assert!(has_text(
            app.world_mut(),
            &format!("Peak horizontal speed · {:.0} units/s", feedback.peak_speed)
        ));
        assert!(has_text(app.world_mut(), "Practice again"));

        choose_menu_action(app, |action| matches!(action, Action::Restart));
        let game = app.world().resource::<Game>();
        assert_eq!(
            (
                game.session.player.position,
                game.session.player.velocity,
                game.session.elapsed
            ),
            entry
        );
        assert!(!game.session.ranked_eligible());
        assert_eq!(game.displayed_elapsed(), 0.0);
        for &command in &demo.commands[entry_tick + 1..] {
            reference.tick_command(&app.world().resource::<Game>().course, command);
            app.world_mut().resource_mut::<Game>().input = command.input;
            app.world_mut().run_schedule(FixedUpdate);
            let game = app.world().resource::<Game>();
            assert_eq!(game.session.player.position, reference.player.position);
            if reference.splits.len() == 2 {
                assert_eq!(
                    game.drill_result(),
                    Some(DrillResult::Cleared(reference.splits[1] - entry.2))
                );
                break;
            }
        }
        app.update();
        assert!(has_text(app.world_mut(), "Section cleared"));
        assert!(app.world().resource::<Game>().finished_replay().is_none());
        choose_menu_action(app, |action| matches!(action, Action::ExitLessonDrill));
        let game = app.world().resource::<Game>();
        assert!(game.drill_section().is_none());
        assert_eq!(game.session.phase, RunPhase::Finished);
        assert_eq!(game.session.elapsed, full_clear);
        assert!(has_text(app.world_mut(), "Lesson complete"));
        assert_eq!(fs::read(progress).unwrap(), saved);
        assert_eq!(menu_training::xp(app.world().resource::<MenuState>()), xp);
    }

    fn clear_bhop_with_live_coach(
        app: &mut App,
        launch: f32,
        expected_checkpoint: &str,
        target: Option<&str>,
    ) -> f64 {
        let mut first_pad = false;
        for _ in 0..400 {
            let mut game = app.world_mut().resource_mut::<Game>();
            let z = game.session.player.position.z;
            game.input = bevy_surf::movement::MoveInput {
                forward: 1.0,
                jump: z <= launch && z > -748.0,
                ..Default::default()
            };
            app.world_mut().run_schedule(FixedUpdate);
            let game = app.world().resource::<Game>();
            if game.session.phase == RunPhase::Finished {
                let elapsed = game.session.elapsed;
                assert!(first_pad);
                assert!(!game.session.ranked_eligible());
                app.update();
                assert_eq!(app.world().resource::<MenuState>().page, Page::Finished);
                return elapsed;
            }
            if !first_pad && game.session.splits.len() == 1 {
                first_pad = true;
                app.update();
                assert!(has_text(app.world_mut(), "Middle hop"));
                assert!(has_text(
                    app.world_mut(),
                    "Keep W held and jump with X as you touch the violet pad."
                ));
                assert!(has_text(app.world_mut(), expected_checkpoint));
                let world = app.world_mut();
                let mut labels = world.query::<&Text>();
                let comparison = labels
                    .iter(world)
                    .find_map(|text| text.0.split_once(" / Last clear ").map(|(_, time)| time));
                assert_eq!(comparison, target);
            }
        }
        panic!("bunnyhop lesson did not finish from launch at {launch}");
    }

    fn open_surf_school(data: &Path) -> App {
        let mut library = MapLibrary::read();
        library.directory = data.to_owned();
        let mut app = App::new();
        app.init_resource::<Assets<Image>>()
            .init_resource::<Clipboard>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<AccumulatedMouseMotion>()
            .init_resource::<AccumulatedMouseScroll>()
            .init_resource::<Online>()
            .init_resource::<Time>()
            .init_resource::<Time<Fixed>>()
            .insert_resource(Game::new(Course::practice()))
            .insert_resource(MenuState::new(data))
            .insert_resource(library)
            .add_message::<KeyboardInput>()
            .add_message::<MenuRequest>()
            .add_systems(
                Update,
                (
                    keyboard,
                    apply_actions,
                    crate::online::update,
                    crate::view::poll_load,
                    render,
                    update_hud,
                )
                    .chain(),
            )
            .add_systems(FixedUpdate, crate::view::advance);
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::F7);
        app.update();
        assert_eq!(app.world().resource::<MenuState>().page, Page::Training);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset_all();
        app
    }

    fn has_text(world: &mut World, expected: &str) -> bool {
        world
            .query::<&Text>()
            .iter(world)
            .any(|text| text.0 == expected)
    }

    fn choose_menu_action(app: &mut App, matches: impl Fn(&Action) -> bool) {
        let world = app.world_mut();
        let action = world
            .query::<&MenuButton>()
            .iter(world)
            .find(|button| matches(&button.0))
            .expect("the menu offers this action")
            .0
            .clone();
        world.resource_mut::<MenuState>().actions.push(action);
        app.update();
    }

    fn start_from_menu(app: &mut App, id: &'static str) {
        let world = app.world_mut();
        let action = world.query::<&MenuButton>().iter(world).find_map(|button| {
            matches!(button.0, Action::Request(MenuRequest::StartLesson(lesson)) if lesson == id)
                .then(|| button.0.clone())
        }).expect("the menu offers this lesson");
        world.resource_mut::<MenuState>().actions.push(action);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            app.update();
            if app
                .world()
                .resource::<Game>()
                .tutorial_lesson()
                .is_some_and(|lesson| lesson.id == id)
            {
                break;
            }
            assert!(Instant::now() < deadline, "lesson failed to load: {id}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
