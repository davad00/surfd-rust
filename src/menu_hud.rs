use super::*;
use crate::view::LiveLessonCue;
use std::{
    fmt::{self, Write},
    time::Instant,
};

const KITSUNE_STAGES: [&str; 9] = [
    "Red", "Orange", "Yellow", "Green", "Teal", "Blue", "Purple", "Pink", "White",
];

pub(super) fn spawn_hud(
    commands: &mut Commands,
    game: &Game,
    settings: &Settings,
    online: &Online,
) {
    let scale = settings.hud_scale;
    let lesson = game.tutorial_lesson();
    let demo = game.watching_lesson_demo();
    let drill = game.drill_section();
    let starter = game.starter_guide() && !game.watching_replay();
    let now = Instant::now();
    let live = (game.has_live_lesson_coach() && !demo).then(|| {
        let [release, steer, switch, stay_high] = [
            "Release {forward} now. Let the drop carry you onto the ramp.",
            "Tap {left} toward the ridge. You’re drifting right.",
            "Release {left}. Steer toward the violet ramp with {right}.",
            "Tap {left} toward the upper stripe. Stay high through the violet bend.",
        ]
        .map(|tip| lesson_bindings(tip, settings));
        Box::new([release, steer, switch, pad_jump_tip(settings), stay_high])
    });
    commands
        .spawn((
            UiRoot,
            Node {
                width: percent(100),
                height: percent(100),
                ..default()
            },
            Pickable::IGNORE,
        ))
        .with_children(|hud| {
            hud.spawn(hud_corner(Some(20.), Some(22.), None, None))
                .with_children(|corner| {
                    corner.spawn(text(
                        lesson.map_or(game.course.name.as_str(), |lesson| lesson.name),
                        14. * scale,
                        INK,
                    ));
                    if online.room_map_time(now).is_some() {
                        corner.spawn((text("", 12. * scale, MINT), Label::RoomMapClock));
                    }
                    if online.is_connected() {
                        corner.spawn(text("Y chat · /help", 11. * scale, MUTED));
                    }
                    if lesson.is_none() {
                        corner.spawn(text("66 tick  /  Source-style surf", 11. * scale, MUTED));
                    }
                    let camera = settings.primary_binding(ControlAction::ToggleCamera);
                    if camera != "—" {
                        corner.spawn(text(
                            format!("{camera}  /  Player camera"),
                            11. * scale,
                            MUTED,
                        ));
                    }
                });
            online_ui::vote_panel(hud, online, now, scale, true);
            hud.spawn(Node {
                align_items: AlignItems::End,
                ..hud_corner(Some(20.), None, Some(22.), None)
            })
            .with_children(|corner| {
                if drill.is_none() {
                    corner.spawn((text("", 14. * scale, INK), Label::Mode));
                }
                if !demo && drill.is_none() {
                    corner.spawn((text("", 11. * scale, MUTED), Label::Best));
                }
                if drill.is_none() && game.displayed_session().phase != RunPhase::Untimed {
                    corner
                        .spawn(Node {
                            column_gap: px(10. * scale),
                            ..default()
                        })
                        .with_children(|route| {
                            let checkpoints = game.displayed_session().checkpoint_count();
                            route.spawn((text("Start", 10. * scale, MINT), Label::Route(0)));
                            for checkpoint in 1..=checkpoints.min(12) {
                                route.spawn((
                                    text(format!("CP {checkpoint}"), 10. * scale, MUTED),
                                    Label::Route(checkpoint),
                                ));
                            }
                            if checkpoints > 12 {
                                route.spawn(text("…", 10. * scale, MUTED));
                            }
                            route.spawn((
                                text("Finish", 10. * scale, MUTED),
                                Label::Route(checkpoints + 1),
                            ));
                        });
                }
            });
            hud.spawn(Node {
                position_type: PositionType::Absolute,
                bottom: percent(12),
                width: percent(100),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: px(2.),
                ..default()
            })
            .with_children(|telemetry| {
                telemetry.spawn((text("00:00.000", 45. * scale, INK), Label::Clock));
                if !settings.center_speed {
                    speed(telemetry, settings);
                }
                if drill.is_none() && game.displayed_session().stage_count(&game.course).is_some() {
                    telemetry.spawn((text("", 14. * scale, INK), Label::StageProgress));
                }
                if drill.is_none() {
                    telemetry.spawn((text("", 12. * scale, MUTED), Label::RunStatus));
                }
                if lesson.is_none() {
                    telemetry.spawn((text("", 11. * scale, MINT), Label::Ranked));
                }
            });
            if lesson.is_some() || starter {
                hud.spawn((
                    Node {
                        bottom: percent(19),
                        width: percent(38),
                        max_width: px(355. * scale),
                        padding: UiRect::all(px(12. * scale)),
                        border: UiRect::left(px(2.)),
                        ..hud_corner(None, Some(22.), None, None)
                    },
                    BackgroundColor(Color::srgba_u8(19, 28, 34, 232)),
                    BorderColor::all(MINT),
                    Pickable::IGNORE,
                ))
                .with_children(|coach| {
                    if let Some(section) = drill {
                        coach.spawn(text(format!("Section practice · {}", section.name), 14. * scale, INK));
                        let tip = lesson_bindings(section.tip, settings);
                        if live.is_some() {
                            coach.spawn((text(&tip, 12. * scale, MUTED), Label::LessonCue { briefing: tip, sections: Box::default(), live }));
                        } else {
                            coach.spawn(text(tip, 12. * scale, MUTED));
                        }
                        coach.spawn(text(lesson_bindings("{restart} restarts this section.", settings), 11. * scale, GOLD));
                        return;
                    }
                    let Some(lesson) = lesson else {
                        let tips = Box::new([
                            "Approach one face of the red ramp with {forward}. Release {forward} as you drop and steer toward the ridge.",
                            "Release {forward} and steer toward the ridge with {left} or {right}.",
                            "Keep your speed for the next ramp. Pair a gentle mouse turn with {left} or {right} to line up the landing.",
                        ].map(|tip| lesson_bindings(tip, settings)));
                        coach.spawn((text("", 14. * scale, INK), Label::StarterStage));
                        coach.spawn((text("", 12. * scale, MUTED), Label::StarterCue(tips)));
                        let retry = if settings.bindings(ControlAction::RestartStage).is_empty() {
                            "Bind Restart current stage in Settings to retry a stage and keep your run timer.".into()
                        } else {
                            format!(
                                "{} retries a stage during a run and keeps your timer.",
                                settings.primary_binding(ControlAction::RestartStage),
                            )
                        };
                        coach.spawn((text(&retry, 11. * scale, GOLD), Label::StarterRetry(retry)));
                        return;
                    };
                    coach.spawn((text(lesson.goal, 14. * scale, INK), Label::LessonHeading));
                    let briefing = lesson_bindings(lesson.instructions, settings);
                    coach.spawn((
                        text(&briefing, 12. * scale, MUTED),
                        Label::LessonCue {
                            briefing,
                            sections: lesson
                                .sections
                                .iter()
                                .map(|section| lesson_bindings(section.tip, settings))
                                .collect(),
                            live,
                        },
                    ));
                    coach.spawn((
                        text("", 11. * scale, GOLD),
                        Label::LessonHint(lesson_bindings(lesson.retry_tip, settings)),
                    ));
                    if demo {
                        button(
                            coach,
                            if game.session.phase == RunPhase::Finished {
                                "Return to results"
                            } else {
                                "Play lesson"
                            },
                            Action::ReturnFromLessonDemo,
                            true,
                        );
                    }
                });
            }
            if settings.show_crosshair {
                hud.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        width: percent(100),
                        height: percent(100),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                    Pickable::IGNORE,
                ))
                .with_children(|center| crosshair(center, settings, 1.0));
            }
            hud.spawn(Node {
                position_type: PositionType::Absolute,
                top: percent(50),
                width: percent(100),
                align_items: AlignItems::Center,
                flex_direction: FlexDirection::Column,
                ..default()
            })
            .with_children(|middle| {
                let style = settings.crosshair;
                let clearance = if settings.show_crosshair {
                    f32::from((style.gap + style.length).max(style.circle) + style.outline)
                } else {
                    0.0
                };
                middle
                    .spawn(Node {
                        margin: UiRect::top(px(clearance + 12. * scale)),
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        row_gap: px(2.),
                        ..default()
                    })
                    .with_children(|below| {
                        if settings.center_speed {
                            speed(below, settings);
                        }
                        if drill.is_none() && (settings.compare_splits || settings.compare_speed) {
                            below.spawn((
                                Node {
                                    margin: UiRect::top(px(32. * scale)),
                                    ..default()
                                },
                                text("", 12. * scale, INK),
                                Label::Splits,
                            ));
                        }
                    });
            });
            if settings.show_keys || demo {
                hud.spawn(Node {
                    bottom: percent(12),
                    ..hud_corner(None, None, Some(28.), None)
                })
                .with_children(|keys| {
                    use ControlAction::*;
                    keys.spawn(Node {
                        justify_content: JustifyContent::Center,
                        ..default()
                    })
                    .with_children(|row| key_cap(row, settings, Forward, 29.));
                    keys.spawn(Node {
                        column_gap: px(4),
                        justify_content: JustifyContent::Center,
                        ..default()
                    })
                    .with_children(|row| {
                        key_cap(row, settings, Left, 29.);
                        key_cap(row, settings, Back, 29.);
                        key_cap(row, settings, Right, 29.);
                    });
                    keys.spawn(Node {
                        column_gap: px(4),
                        ..default()
                    })
                    .with_children(|row| {
                        key_cap(row, settings, Jump, 42.);
                        key_cap(row, settings, Crouch, 53.);
                    });
                });
            }
            hud.spawn((
                hud_corner(None, None, Some(22.), Some(16.)),
                text("", 11. * scale, MUTED),
                Label::Shortcut,
            ));
            hud.spawn((
                Node {
                    bottom: px(46. * scale),
                    max_width: percent(70),
                    ..hud_corner(None, Some(22.), None, None)
                },
                text("", 12. * scale, MINT),
                Label::Notice,
            ));
            hud.spawn((
                hud_corner(Some(78.), Some(22.), None, None),
                text("", 12. * scale, MINT),
                Label::Ghost,
            ));
            if settings.diagnostics {
                hud.spawn((
                    hud_corner(None, Some(22.), None, Some(16.)),
                    text("", 12. * scale, MINT),
                    Label::Diagnostics,
                ));
            }
            hud.spawn((
                Node {
                    bottom: percent(13),
                    max_width: percent(37),
                    ..hud_corner(None, Some(22.), None, None)
                },
                text("", 12. * scale, INK),
                Label::Chat,
            ));
        });
}

fn speed(parent: &mut ChildSpawnerCommands, settings: &Settings) {
    parent.spawn((
        text("0 units/s", 18. * settings.hud_scale, INK),
        Label::Speed,
    ));
    if settings.show_sync {
        parent.spawn((text("Sync —", 12. * settings.hud_scale, MUTED), Label::Sync));
    }
}

pub(super) fn lesson_bindings(message: &str, settings: &Settings) -> String {
    let mut message = message.to_owned();
    for (token, action) in [
        ("{forward}", ControlAction::Forward),
        ("{left}", ControlAction::Left),
        ("{right}", ControlAction::Right),
        ("{jump}", ControlAction::Jump),
        ("{restart}", ControlAction::Restart),
    ] {
        if message.contains(token) {
            message = message.replace(token, &settings.primary_binding(action));
        }
    }
    message
}

pub(super) fn pad_jump_tip(settings: &Settings) -> String {
    let bindings = settings.bindings(ControlAction::Jump);
    if let Some(binding) = bindings
        .iter()
        .find(|binding| matches!(binding, Binding::Key(_) | Binding::Mouse(_)))
    {
        return format!(
            "Hold {} as you land on the violet pad to hop immediately.",
            binding.label()
        );
    }
    match bindings.first() {
        Some(Binding::WheelUp) => "Scroll up as you touch the violet pad to hop again.".into(),
        Some(Binding::WheelDown) => "Scroll down as you touch the violet pad to hop again.".into(),
        _ => "Bind Jump in Settings so you can hop off the violet pad.".into(),
    }
}

pub(super) fn crosshair(parent: &mut ChildSpawnerCommands, settings: &Settings, scale: f32) {
    let style = settings.crosshair;
    let color = style.color(settings.colorblind);
    let length = f32::from(style.length) * scale;
    let width = f32::from(style.thickness) * scale;
    let gap = f32::from(style.gap) * scale;
    let dot = f32::from(style.dot) * scale;
    let outline = f32::from(style.outline) * scale;
    let half = width / 2.0;
    let mut parts = Vec::with_capacity(5);
    if length > 0.0 {
        if gap == 0.0 {
            parts.push([-half - length, -half, width + 2.0 * length, width]);
            let up = if style.t_shape { 0.0 } else { length };
            parts.push([-half, -half - up, width, width + length + up]);
        } else {
            parts.push([-half - gap - length, -half, length, width]);
            parts.push([half + gap, -half, length, width]);
            if !style.t_shape {
                parts.push([-half, -half - gap - length, width, length]);
            }
            parts.push([-half, half + gap, width, length]);
        }
    }
    if dot > 0.0 {
        parts.push([-dot / 2.0, -dot / 2.0, dot, dot]);
    }
    parent
        .spawn((
            Node {
                width: px(0),
                height: px(0),
                ..default()
            },
            Pickable::IGNORE,
        ))
        .with_children(|anchor| {
            let radius = f32::from(style.circle) * scale;
            let stroke = f32::from(style.circle_width) * scale;
            if outline > 0.0 {
                for [x, y, w, h] in &parts {
                    rectangle(
                        anchor,
                        x - outline,
                        y - outline,
                        w + 2.0 * outline,
                        h + 2.0 * outline,
                        Color::BLACK,
                    );
                }
                if radius > 0.0 {
                    ring(anchor, radius, stroke + 2.0 * outline, Color::BLACK);
                }
            }
            for [x, y, w, h] in parts {
                rectangle(anchor, x, y, w, h, color);
            }
            if radius > 0.0 {
                ring(anchor, radius, stroke, color);
            }
        });
}

fn rectangle(parent: &mut ChildSpawnerCommands, x: f32, y: f32, w: f32, h: f32, color: Color) {
    parent.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: px(x),
            top: px(y),
            width: px(w),
            height: px(h),
            ..default()
        },
        BackgroundColor(color),
        Pickable::IGNORE,
    ));
}

fn ring(parent: &mut ChildSpawnerCommands, radius: f32, width: f32, color: Color) {
    let edge = radius + width / 2.0;
    parent.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: px(-edge),
            top: px(-edge),
            width: px(edge * 2.0),
            height: px(edge * 2.0),
            border: UiRect::all(px(width)),
            border_radius: BorderRadius::MAX,
            ..default()
        },
        BorderColor::all(color),
        Pickable::IGNORE,
    ));
}

fn key_cap(
    parent: &mut ChildSpawnerCommands,
    settings: &Settings,
    action: ControlAction,
    width: f32,
) {
    let name = match action {
        ControlAction::Jump => "Jump".into(),
        ControlAction::Crouch => "Crouch".into(),
        _ => settings.primary_binding(action),
    };
    let scale = settings.hud_scale;
    parent
        .spawn((
            Node {
                min_width: px(width * scale),
                height: px(25. * scale),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                padding: UiRect::horizontal(px(4)),
                border: UiRect::all(px(1)),
                ..default()
            },
            BackgroundColor(INSET),
            BorderColor::all(EDGE),
            Pickable::IGNORE,
            KeyCap(action),
        ))
        .with_child(text(name, 11. * scale, INK));
}

#[derive(Default)]
pub struct HudRecordKey {
    state: Option<(u64, Option<u16>, RunOptions)>,
    key: String,
}

#[derive(Default)]
pub struct HudStageProgress {
    progress: Option<(usize, usize)>,
    label: String,
}

#[derive(Default)]
pub struct HudText {
    scratch: String,
    restart_binding: Option<Binding>,
    restart_label: String,
}

pub fn update_hud(
    game: Res<Game>,
    menu: Res<MenuState>,
    online: Res<Online>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    wheel: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    mut labels: Query<(&Label, &mut Text, &mut TextColor)>,
    mut caps: Query<(&KeyCap, &mut BackgroundColor, &mut BorderColor)>,
    mut vote_panels: Query<&mut Node, With<online_ui::VotePanel>>,
    mut record_key: Local<HudRecordKey>,
    mut stage_progress: Local<HudStageProgress>,
    mut hud_text: Local<HudText>,
) {
    if labels.is_empty() && caps.is_empty() && vote_panels.is_empty() {
        return;
    }
    let HudText {
        scratch,
        restart_binding,
        restart_label,
    } = &mut *hud_text;
    let run = game.displayed_session();
    let options = run.options();
    let lesson = game.tutorial_lesson();
    let demo = game.watching_lesson_demo();
    let last_clear = lesson
        .filter(|_| !demo)
        .and_then(|lesson| menu.training.last_clear(lesson.id));
    let active_section = lesson
        .filter(|_| run.phase == RunPhase::Running && (demo || !game.watching_replay()))
        .and_then(|lesson| {
            let index = run.splits.len();
            lesson.sections.get(index).map(|section| (index, section))
        });
    let state = (
        game.course_version,
        game.selected_bonus(),
        game.session.options(),
    );
    if record_key.state != Some(state) {
        record_key.state = Some(state);
        record_key.key = menu.record_key(&game);
    }
    let best = menu.records.get(&record_key.key);
    let world = online
        .board
        .as_ref()
        .filter(|board| {
            board.stage.is_none()
                && board.map == game.course.name
                && board.bonus == game.selected_bonus()
        })
        .and_then(|board| board.rows.first());
    let now = Instant::now();
    let vote_seconds = online.active_vote(now).map(|(_, seconds)| seconds);
    if vote_seconds.is_none() {
        for mut panel in &mut vote_panels {
            if panel.display != Display::None {
                panel.display = Display::None;
            }
        }
    }
    for (label, mut text, mut color) in &mut labels {
        scratch.clear();
        let value: &str = match label {
            Label::Notice => {
                if menu.page == Page::Playing && now >= menu.notice_until {
                    ""
                } else {
                    &menu.notice
                }
            }
            Label::Clock => {
                let _ = write!(scratch, "{}", HudTime(game.displayed_elapsed()));
                scratch.as_str()
            }
            Label::Mode => {
                if demo {
                    "LESSON DEMO"
                } else if game.watching_replay() {
                    "REPLAY"
                } else if lesson.is_some() {
                    "SURF SCHOOL"
                } else if game.practice_name.is_some() {
                    "PRACTICE"
                } else {
                    match (options.unrestricted_start, options.auto_bhop) {
                        (false, false) => "NORMAL + MANUAL BHOP",
                        (false, true) => "NORMAL + AUTO BHOP",
                        (true, false) => "UNRESTRICTED START + MANUAL BHOP",
                        (true, true) => "UNRESTRICTED START + AUTO BHOP",
                    }
                }
            }
            Label::Best => {
                let local = if let Some(lesson) = lesson {
                    menu.training.best(lesson.id)
                } else {
                    best.map(|run| run.seconds)
                };
                let best = run.best.into_iter().chain(local).reduce(f64::min);
                if let Some(seconds) = best {
                    let _ = write!(scratch, "Personal best: {}", HudTime(seconds));
                    scratch.as_str()
                } else {
                    "Personal best: —"
                }
            }
            Label::Speed => {
                let velocity = run.player.velocity;
                let speed = f64::from(velocity.x).hypot(f64::from(velocity.z));
                let _ = write!(scratch, "{:.0} units/s", speed.round());
                scratch.as_str()
            }
            Label::StageProgress => {
                let progress = game.displayed_stage_progress();
                if stage_progress.progress != progress {
                    stage_progress.progress = progress;
                    stage_progress.label.clear();
                    if let Some((stage, total)) = progress {
                        let _ = write!(stage_progress.label, "Stage {stage}/{total}");
                    }
                }
                &stage_progress.label
            }
            Label::RunStatus => match run.phase {
                RunPhase::Untimed => {
                    if let Some(name) = game.practice_name.as_deref() {
                        let _ = write!(scratch, "Practice  /  {name}");
                        scratch.as_str()
                    } else {
                        "Free surf"
                    }
                }
                RunPhase::Ready => "In start zone",
                RunPhase::Running => {
                    if let Some(number) = run.selected_bonus() {
                        let _ = write!(scratch, "Bonus {number}");
                    }
                    scratch.as_str()
                }
                RunPhase::Finished => "Finished",
            },
            Label::Sync => {
                if let Some(sync) = run.strafe_sync() {
                    let _ = write!(scratch, "Sync {:.1}%", sync * 100.0);
                    scratch.as_str()
                } else {
                    "Sync —"
                }
            }
            Label::Route(index) => {
                let current = if run.phase == RunPhase::Finished {
                    run.checkpoint_count() + 1
                } else {
                    run.splits.len()
                };
                let target = if *index == current {
                    MINT
                } else if *index < current {
                    INK
                } else {
                    MUTED
                };
                if color.0 != target {
                    color.0 = target;
                }
                continue;
            }
            Label::Splits => {
                let Some(index) = run.splits.len().checked_sub(1) else {
                    if !text.0.is_empty() {
                        text.0.clear();
                    }
                    continue;
                };
                if let Some(lesson) = lesson {
                    write_lesson_checkpoint(
                        scratch,
                        lesson,
                        run,
                        index,
                        last_clear,
                        &menu.settings,
                    );
                } else {
                    let split = run.splits[index];
                    let _ = write!(scratch, "CHECKPOINT {}  ·  {}", index + 1, HudTime(split));
                    if menu.settings.compare_splits {
                        let stage_start = if index > 0 {
                            run.splits[index - 1]
                        } else {
                            0.0
                        };
                        let pb = best.and_then(|best| best.splits.get(index).copied());
                        let wr = world
                            .and_then(|row| row.splits_us.get(index).copied())
                            .map(micros);
                        let pb_start = best
                            .and_then(|row| {
                                index
                                    .checked_sub(1)
                                    .and_then(|i| row.splits.get(i).copied())
                            })
                            .unwrap_or(0.0);
                        let wr_start = world
                            .and_then(|row| {
                                index
                                    .checked_sub(1)
                                    .and_then(|i| row.splits_us.get(i).copied())
                            })
                            .map(micros)
                            .unwrap_or(0.0);
                        let _ = write!(
                            scratch,
                            "\nSTAGE   PB {}    WR {}",
                            HudDelta(pb.map(|s| split - stage_start - (s - pb_start))),
                            HudDelta(wr.map(|s| split - stage_start - (s - wr_start)))
                        );
                        let _ = write!(
                            scratch,
                            "\nRUN      PB {}    WR {}",
                            HudDelta(pb.map(|s| split - s)),
                            HudDelta(wr.map(|s| split - s))
                        );
                    }
                    if menu.settings.compare_speed
                        && let Some(speed) = run.checkpoint_speeds.get(index)
                    {
                        let _ = write!(scratch, "\n{speed:.0} units/s   WR ");
                        if let Some(reference) =
                            world.and_then(|row| row.checkpoint_speeds.get(index))
                        {
                            let _ = write!(scratch, "{:+.0}", speed - reference);
                        } else {
                            scratch.push('—');
                        }
                    }
                }
                scratch.as_str()
            }
            Label::Diagnostics => {
                let p = run.player.position;
                let v = run.player.velocity;
                let _ = write!(
                    scratch,
                    "pos {:8.1} {:8.1} {:8.1}\nvel {:8.1} {:8.1} {:8.1}\nframe {:5.1} ms",
                    p.x,
                    p.y,
                    p.z,
                    v.x,
                    v.y,
                    v.z,
                    time.delta_secs() * 1000.0
                );
                scratch.as_str()
            }
            Label::Chat => {
                use crate::online::Field;
                let lines = online
                    .messages
                    .iter()
                    .skip(online.messages.len().saturating_sub(4));
                let has_messages = !online.messages.is_empty();
                for (index, line) in lines.enumerate() {
                    if index != 0 {
                        scratch.push('\n');
                    }
                    scratch.push_str(line);
                }
                if online.field == Some(Field::Chat) {
                    let input = online.field_value(Field::Chat);
                    let hint = if input.is_empty() {
                        if online.managed_guest() {
                            "Command or /help"
                        } else {
                            "Message or /help"
                        }
                    } else {
                        input
                    };
                    if has_messages {
                        scratch.push('\n');
                    }
                    scratch.push_str(hint);
                    scratch.push('▏');
                }
                scratch.as_str()
            }
            Label::RoomMapClock => {
                if let Some(value) = online.room_map_time(now) {
                    scratch.push_str(&value);
                }
                scratch.as_str()
            }
            Label::RoomVoteClock => {
                if let Some(seconds) = vote_seconds {
                    let _ = write!(scratch, "{seconds}s");
                }
                scratch.as_str()
            }
            Label::Ranked => &online.record_status,
            Label::LessonHeading => active_section
                .map(|(_, section)| section.name)
                .or_else(|| lesson.map(|lesson| lesson.goal))
                .unwrap_or_default(),
            Label::LessonCue {
                briefing,
                sections,
                live,
            } => {
                let live = live.as_deref().and_then(|tips| {
                    game.live_lesson_cue().map(|cue| match cue {
                        LiveLessonCue::ReleaseForward => &tips[0],
                        LiveLessonCue::SteerLeft => &tips[1],
                        LiveLessonCue::SwitchRight => &tips[2],
                        LiveLessonCue::HopOnLanding => &tips[3],
                        LiveLessonCue::StayHigh => &tips[4],
                    })
                });
                let (cue, tint) = live.map_or_else(
                    || {
                        (
                            active_section
                                .and_then(|(index, _)| sections.get(index))
                                .unwrap_or(briefing),
                            MUTED,
                        )
                    },
                    |cue| (cue, GOLD),
                );
                if color.0 != tint {
                    color.0 = tint;
                }
                cue
            }
            Label::LessonHint(retry) => {
                if !demo && game.lesson_retries > 0 && run.phase == RunPhase::Ready {
                    retry
                } else if menu.settings.compare_splits
                    && let (Some((index, _)), Some(last)) = (active_section, last_clear)
                {
                    let section_elapsed =
                        run.elapsed - menu_training::section_start(&run.splits, index);
                    let _ = write!(
                        scratch,
                        "Section {} / Last clear {}",
                        HudTime(section_elapsed),
                        HudTime(menu_training::section_time(last, index)),
                    );
                    scratch.as_str()
                } else {
                    ""
                }
            }
            Label::StarterStage => {
                if run.phase == RunPhase::Untimed {
                    "Kitsune · Stage practice"
                } else {
                    let count = run.checkpoint_count() + 1;
                    let stage = run.splits.len().min(count - 1);
                    let _ = write!(scratch, "Kitsune · Stage {}/{count}", stage + 1);
                    if let Some(name) = KITSUNE_STAGES.get(stage) {
                        let _ = write!(scratch, " · {name}");
                    }
                    scratch.as_str()
                }
            }
            Label::StarterCue(tips) => match run.phase {
                RunPhase::Untimed => {
                    "Open Go to stage from the pause menu to choose a practice start."
                }
                RunPhase::Ready => &tips[0],
                RunPhase::Running if run.splits.is_empty() => {
                    if !run.player.grounded && game.displayed_input().forward > 0.0 {
                        &tips[1]
                    } else {
                        "Look along the ramp and make short taps toward the ridge when you drift."
                    }
                }
                RunPhase::Running => &tips[2],
                RunPhase::Finished => "Kitsune complete.",
            },
            Label::StarterRetry(retry) => {
                if run.phase == RunPhase::Untimed {
                    ""
                } else {
                    retry
                }
            }
            Label::Ghost => {
                if let Some((done, total)) = game.ghost_preparation_progress() {
                    let percent = if total == 0 { 0 } else { done * 100 / total };
                    let _ = write!(scratch, "Preparing ghost  {percent}%");
                    scratch.as_str()
                } else if game.ghost_enabled() {
                    "Record Ghost"
                } else {
                    ""
                }
            }
            Label::Shortcut => {
                if let Some((tick, total, paused)) = game.replay_progress() {
                    let back = if demo {
                        "Backspace return"
                    } else {
                        "Backspace exit"
                    };
                    let _ = write!(
                        scratch,
                        "Replay {tick}/{total}{}\nSpace pause · Right step · R rewind · {back}",
                        if paused { " · Paused" } else { "" }
                    );
                    scratch.as_str()
                } else {
                    let binding = menu.settings.bindings(ControlAction::Restart).first();
                    if restart_label.is_empty() || restart_binding.as_ref() != binding {
                        *restart_binding = binding.cloned();
                        restart_label.clear();
                        let binding = menu.settings.primary_binding(ControlAction::Restart);
                        let _ = write!(restart_label, "{binding} restart   Esc menu");
                    }
                    restart_label.as_str()
                }
            }
        };
        if text.0 != value {
            text.0.clear();
            text.0.push_str(value);
        }
    }
    let input = game.displayed_input();
    for (key, mut color, mut border) in &mut caps {
        let pressed = if game.watching_replay() {
            match key.0 {
                ControlAction::Forward => input.forward > 0.0,
                ControlAction::Back => input.forward < 0.0,
                ControlAction::Right => input.strafe > 0.0,
                ControlAction::Left => input.strafe < 0.0,
                ControlAction::Jump => input.jump,
                ControlAction::Crouch => input.crouch,
                _ => false,
            }
        } else {
            menu.settings.active(key.0, &keys, &mouse, wheel.delta)
        };
        let active = if menu.settings.colorblind {
            Color::srgb_u8(36, 82, 132)
        } else {
            ACTIVE
        };
        let background = if pressed { active } else { INSET };
        if color.0 != background {
            color.0 = background;
            *border = BorderColor::all(if pressed { MINT } else { EDGE });
        }
    }
}

fn write_lesson_checkpoint(
    label: &mut String,
    lesson: &bevy_surf::tutorial::Lesson,
    run: &bevy_surf::session::Session,
    index: usize,
    last: Option<&BestRun>,
    settings: &Settings,
) {
    let Some(section) = lesson.sections.get(index) else {
        return;
    };
    label.push_str(section.name);
    if settings.compare_splits {
        let seconds = run.splits[index] - menu_training::section_start(&run.splits, index);
        let _ = write!(label, " · {}", HudTime(seconds));
        if let Some(last) = last {
            let difference = seconds - menu_training::section_time(last, index);
            if difference.abs() < 0.0005 {
                label.push_str(" (Even vs last clear)");
            } else {
                let _ = write!(label, " ({difference:+.3}s vs last clear)");
            }
        }
    }
    if settings.compare_speed
        && let Some(speed) = run.checkpoint_speeds.get(index)
    {
        let _ = write!(label, "\n{speed:.0} units/s");
        if let Some(previous) = last.and_then(|last| last.speeds.get(index)) {
            let difference = speed - previous;
            if difference.abs() >= 0.5 {
                let _ = write!(label, " ({difference:+.0} vs last clear)");
            }
        }
    }
}

fn micros(value: u64) -> f64 {
    value as f64 / 1_000_000.0
}

struct HudTime(f64);

impl fmt::Display for HudTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let millis = (self.0 * 1000.0).round() as u64;
        write!(
            formatter,
            "{:02}:{:02}.{:03}",
            millis / 60_000,
            millis / 1000 % 60,
            millis % 1000
        )
    }
}

struct HudDelta(Option<f64>);

impl fmt::Display for HudDelta {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(value) => write!(formatter, "{value:+.3}s"),
            None => formatter.write_str("—"),
        }
    }
}
