use super::*;
use crate::library::STARTER_MAP;
use crate::view::{DrillFeedback, DrillFinding, DrillResult};
use bevy_surf::{
    catalog,
    tutorial::{self, Medal},
};
use std::collections::BTreeSet;

const XP_PER_LEVEL: usize = 500;
const COURSE_XP: usize = 200;
const BONUS_XP: usize = 100;

pub(super) struct PathCourse {
    id: &'static str,
    goal: &'static str,
}

const COURSE_PATH: [PathCourse; 3] = [
    PathCourse {
        id: STARTER_MAP,
        goal: "Clear nine colour-coded stages with the ramp and stage-retry guide.",
    },
    PathCourse {
        id: "surf_tendies",
        goal: "Carry your speed through curved ramps and four checkpoints in one continuous run.",
    },
    PathCourse {
        id: "surf_rookie",
        goal: "Apply your transfers to 18 stages on your first Tier 2 course.",
    },
];

impl PathCourse {
    fn name(&self) -> &str {
        catalog::find(self.id).map_or(self.id, |entry| entry.name.as_str())
    }
}

fn medal_xp(medal: Medal) -> usize {
    match medal {
        Medal::Bronze => 100,
        Medal::Silver => 130,
        Medal::Gold => 180,
    }
}

fn medal_label(medal: Medal) -> &'static str {
    match medal {
        Medal::Bronze => "Bronze",
        Medal::Silver => "Silver",
        Medal::Gold => "Gold",
    }
}

fn medal_color(medal: Medal) -> Color {
    match medal {
        Medal::Bronze => Color::srgb_u8(230, 164, 129),
        Medal::Silver => INK,
        Medal::Gold => GOLD,
    }
}

#[derive(Default, serde::Serialize)]
struct SavedLesson {
    best: Option<f64>,
    last: Option<BestRun>,
}

impl SavedLesson {
    fn read(path: &Path, lesson: &tutorial::Lesson) -> std::io::Result<Option<Self>> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        // Earlier clients saved an empty completion marker or a plain time.
        let mut saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
        let best = saved
            .get("best")
            .and_then(serde_json::Value::as_f64)
            .or_else(|| std::str::from_utf8(&bytes).ok()?.trim().parse().ok())
            .filter(|&seconds| lesson.medal(seconds).is_some());
        let last = saved
            .get_mut("last")
            .and_then(|last| serde_json::from_value::<BestRun>(last.take()).ok())
            .filter(|run| valid_sections(lesson, run.seconds, &run.splits, &run.speeds));
        Ok(Some(Self { best, last }))
    }

    fn medal(&self, lesson: &tutorial::Lesson) -> Medal {
        self.best
            .and_then(|best| lesson.medal(best))
            .unwrap_or(Medal::Bronze)
    }
}

pub(super) struct LessonResult {
    medal: Medal,
    personal_best: bool,
    xp: usize,
    sections: Vec<SectionResult>,
}

struct SectionResult {
    seconds: f64,
    delta: Option<f64>,
    speed: Option<f32>,
    speed_delta: Option<f32>,
}

fn valid_sections(lesson: &tutorial::Lesson, seconds: f64, splits: &[f64], speeds: &[f32]) -> bool {
    lesson.medal(seconds).is_some()
        && lesson.sections.len() == splits.len() + 1
        && speeds.len() == splits.len()
        && speeds
            .iter()
            .all(|speed| speed.is_finite() && *speed >= 0.0)
        && std::iter::once(0.0)
            .chain(splits.iter().copied())
            .zip(splits.iter().copied().chain(std::iter::once(seconds)))
            .all(|(start, end)| end.is_finite() && end > start)
}

pub(super) fn section_start(splits: &[f64], index: usize) -> f64 {
    index
        .checked_sub(1)
        .and_then(|i| splits.get(i))
        .copied()
        .unwrap_or(0.0)
}

pub(super) fn section_time(run: &BestRun, index: usize) -> f64 {
    run.splits.get(index).copied().unwrap_or(run.seconds) - section_start(&run.splits, index)
}

pub(super) fn section_delta(delta: f64) -> String {
    if delta.abs() < 0.0005 {
        "Even".into()
    } else {
        format!("{delta:+.3}s")
    }
}

fn section_to_practice(sections: &[SectionResult]) -> Option<usize> {
    sections
        .iter()
        .enumerate()
        .filter_map(|(index, section)| section.delta.map(|delta| (index, delta)))
        .filter(|&(_, delta)| delta >= 0.0005)
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(index, _)| index)
}

pub(super) struct TrainingProgress {
    directory: PathBuf,
    completed: BTreeMap<&'static str, SavedLesson>,
}

impl TrainingProgress {
    pub(super) fn read(data: &Path) -> Self {
        let directory = data.join("training");
        let completed = tutorial::LESSONS
            .iter()
            .filter_map(|lesson| {
                let saved = SavedLesson::read(&directory.join(lesson.id), lesson).ok()??;
                Some((lesson.id, saved))
            })
            .collect();
        Self {
            directory,
            completed,
        }
    }

    fn complete_lesson(&self, id: &str) -> bool {
        self.completed.contains_key(id)
    }

    pub(super) fn best(&self, id: &str) -> Option<f64> {
        self.completed.get(id).and_then(|saved| saved.best)
    }

    pub(super) fn last_clear(&self, id: &str) -> Option<&BestRun> {
        self.completed.get(id)?.last.as_ref()
    }

    pub(super) fn next(&self) -> Option<&'static tutorial::Lesson> {
        tutorial::LESSONS
            .iter()
            .find(|lesson| !self.complete_lesson(lesson.id))
    }

    pub(super) fn unlocked(&self, id: &str) -> bool {
        self.complete_lesson(id) || self.next().is_some_and(|lesson| lesson.id == id)
    }

    pub(super) fn record(
        &mut self,
        lesson: &tutorial::Lesson,
        seconds: f64,
        splits: &[f64],
        speeds: &[f32],
    ) -> std::io::Result<LessonResult> {
        let medal = lesson
            .medal(seconds)
            .ok_or(std::io::ErrorKind::InvalidData)?;
        fs::create_dir_all(&self.directory)?;
        let lock = fs::File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join(".lock"))?;
        lock.lock()?;
        let path = self.directory.join(lesson.id);
        let previous = SavedLesson::read(&path, lesson)?;
        let previous_xp = previous
            .as_ref()
            .map_or(0, |saved| medal_xp(saved.medal(lesson)));
        let previous = previous.unwrap_or_default();
        let previous_best = previous.best;
        let personal_best = previous_best.is_none_or(|best| seconds < best);
        let attempt = valid_sections(lesson, seconds, splits, speeds).then(|| BestRun {
            seconds,
            splits: splits.to_vec(),
            speeds: speeds.to_vec(),
        });
        let sections = attempt.as_ref().map_or_else(Vec::new, |run| {
            (0..lesson.sections.len())
                .map(|index| {
                    let seconds = section_time(run, index);
                    let speed = run.speeds.get(index).copied();
                    SectionResult {
                        seconds,
                        delta: previous
                            .last
                            .as_ref()
                            .map(|last| seconds - section_time(last, index)),
                        speed,
                        speed_delta: speed
                            .zip(
                                previous
                                    .last
                                    .as_ref()
                                    .and_then(|last| last.speeds.get(index)),
                            )
                            .map(|(current, previous)| current - previous),
                    }
                })
                .collect()
        });
        let write = personal_best || attempt.is_some();
        let saved = SavedLesson {
            best: Some(previous_best.map_or(seconds, |best| best.min(seconds))),
            last: attempt.or(previous.last),
        };
        if write {
            let temporary = path.with_extension("tmp");
            fs::write(&temporary, serde_json::to_vec(&saved)?)?;
            if let Err(error) = fs::rename(&temporary, &path) {
                let _ = fs::remove_file(temporary);
                return Err(error);
            }
        }
        self.completed.insert(lesson.id, saved);
        Ok(LessonResult {
            medal,
            personal_best,
            xp: medal_xp(medal).saturating_sub(previous_xp),
            sections,
        })
    }

    fn xp(&self) -> usize {
        tutorial::LESSONS
            .iter()
            .filter_map(|lesson| {
                self.completed
                    .get(lesson.id)
                    .map(|saved| medal_xp(saved.medal(lesson)))
            })
            .sum()
    }
}

struct Journey {
    courses: BTreeMap<&'static str, f64>,
    bonuses: usize,
    xp: usize,
}

impl Journey {
    fn new(training: &TrainingProgress, records: &BTreeMap<String, BestRun>) -> Self {
        let mut courses = BTreeMap::<&'static str, f64>::new();
        let mut bonuses = BTreeSet::new();
        for (key, record) in records {
            if !record.seconds.is_finite() || record.seconds <= 0.0 {
                continue;
            }
            if let Some(entry) = catalog::find(key) {
                courses
                    .entry(entry.id.as_str())
                    .and_modify(|best| *best = (*best).min(record.seconds))
                    .or_insert(record.seconds);
            } else if let Some((map, number)) = key.rsplit_once(" · Bonus ")
                && let Some(entry) = catalog::find(map)
                && let Ok(number) = number.parse::<usize>()
                && (1..=entry.bonuses).contains(&number)
            {
                bonuses.insert((entry.id.as_str(), number));
            }
        }
        let bonuses = bonuses.len();
        let xp = training.xp() + courses.len() * COURSE_XP + bonuses * BONUS_XP;
        Self {
            courses,
            bonuses,
            xp,
        }
    }

    fn path_clears(&self) -> usize {
        COURSE_PATH
            .iter()
            .filter(|step| self.courses.contains_key(step.id))
            .count()
    }

    fn next_path_course(&self) -> Option<&'static PathCourse> {
        COURSE_PATH
            .iter()
            .find(|step| !self.courses.contains_key(step.id))
    }
}

pub(super) fn xp(menu: &MenuState) -> usize {
    Journey::new(&menu.training, &menu.records).xp
}

pub(super) fn path_course_available(menu: &MenuState, id: &str) -> bool {
    if menu.training.next().is_some() || !COURSE_PATH.iter().any(|step| step.id == id) {
        return false;
    }
    let journey = Journey::new(&menu.training, &menu.records);
    journey.courses.contains_key(id) || journey.next_path_course().is_some_and(|step| step.id == id)
}

pub(super) fn next_path_course(menu: &MenuState) -> Option<&'static PathCourse> {
    if menu.training.next().is_some() {
        return None;
    }
    Journey::new(&menu.training, &menu.records).next_path_course()
}

pub(super) fn finished_path(
    menu: &MenuState,
    game: &Game,
) -> Option<(usize, Option<&'static PathCourse>)> {
    if menu.training.next().is_some()
        || game.course.catalog_revision.is_none()
        || game.selected_bonus().is_some()
        || game.session.options() != RunOptions::default()
        || !COURSE_PATH.iter().any(|step| step.id == game.course.name)
    {
        return None;
    }
    let journey = Journey::new(&menu.training, &menu.records);
    journey
        .courses
        .contains_key(game.course.name.as_str())
        .then(|| (journey.path_clears(), journey.next_path_course()))
}

pub(super) fn path_summary(clears: usize) -> String {
    if clears == COURSE_PATH.len() {
        "Course path complete".into()
    } else {
        format!("Course path · {clears}/{} cleared", COURSE_PATH.len())
    }
}

pub(super) fn path_button(parent: &mut ChildSpawnerCommands, step: &PathCourse) {
    let title = if step.id == STARTER_MAP {
        "Start guided Kitsune".into()
    } else {
        format!("Play {}", step.name())
    };
    button(
        parent,
        &title,
        Action::Request(MenuRequest::StartPathCourse(step.id)),
        true,
    );
}

pub(super) fn render(parent: &mut ChildSpawnerCommands, menu: &MenuState, game: &Game) {
    let journey = Journey::new(&menu.training, &menu.records);
    let level = 1 + journey.xp / XP_PER_LEVEL;
    let next = menu.training.next();
    title(
        parent,
        "SURF SCHOOL",
        "Learn to surf",
        format!(
            "{} of {} lessons complete",
            menu.training.completed.len(),
            tutorial::LESSONS.len()
        ),
    );
    parent
        .spawn(Node {
            column_gap: px(10.),
            row_gap: px(8.),
            align_items: AlignItems::Center,
            flex_wrap: FlexWrap::Wrap,
            ..default()
        })
        .with_children(|summary| {
            summary.spawn(text(
                format!("Level {level}  /  {} XP", journey.xp),
                16.,
                GOLD,
            ));
            summary.spawn(text(
                format!(
                    "{} / {} XP toward next level   |   {} map clears   |   {} bonus clears",
                    journey.xp % XP_PER_LEVEL,
                    XP_PER_LEVEL,
                    journey.courses.len(),
                    journey.bonuses,
                ),
                12.,
                MUTED,
            ));
        });
    parent
        .spawn((
            Node {
                width: percent(100),
                max_width: px(680.),
                height: px(4.),
                ..default()
            },
            BackgroundColor(INSET),
        ))
        .with_children(|track| {
            track.spawn((
                Node {
                    width: percent((journey.xp % XP_PER_LEVEL) as f32 * 100. / XP_PER_LEVEL as f32),
                    height: percent(100),
                    ..default()
                },
                BackgroundColor(GOLD),
            ));
        });
    parent.spawn((text(&menu.notice, 12., MINT), Label::Notice));
    if next.is_none() {
        parent
            .spawn((
                Node {
                    width: percent(100.),
                    max_width: px(680.),
                    padding: UiRect::all(px(11.)),
                    row_gap: px(5.),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Start,
                    ..default()
                },
                BackgroundColor(INSET),
            ))
            .with_children(|card| {
                let next = journey.next_path_course();
                card.spawn(text(path_summary(journey.path_clears()), 16., INK));
                for step in &COURSE_PATH {
                    let Some(entry) = catalog::find(step.id) else {
                        continue;
                    };
                    let tier = entry
                        .tier
                        .map_or_else(String::new, |tier| format!(" · Tier {tier}"));
                    let status = if let Some(best) = journey.courses.get(step.id) {
                        format!("Clear {}", format_time(*best))
                    } else if next.is_some_and(|next| next.id == step.id) {
                        "Next".into()
                    } else {
                        "Later".into()
                    };
                    card.spawn(text(format!("{}{tier} · {status}", entry.name), 13., MUTED));
                }
                if let Some(next) = next {
                    card.spawn(text(next.goal, 12., INK));
                    path_button(card, next);
                } else {
                    button(card, "Choose another map", Action::Open(Page::Maps), true);
                }
                if journey.courses.contains_key(STARTER_MAP) {
                    button(
                        card,
                        "Replay guided Kitsune",
                        Action::Request(MenuRequest::StartPathCourse(STARTER_MAP)),
                        false,
                    );
                }
            });
    }
    parent
        .spawn(Node {
            width: percent(100),
            max_width: px(870.),
            column_gap: px(10.),
            row_gap: px(10.),
            flex_wrap: FlexWrap::Wrap,
            ..default()
        })
        .with_children(|grid| {
            for (index, lesson) in tutorial::LESSONS.iter().enumerate() {
                let current = next.is_some_and(|next| next.id == lesson.id);
                let saved = menu.training.completed.get(lesson.id);
                let in_progress = game.session.phase != RunPhase::Finished
                    && game
                        .tutorial_lesson()
                        .is_some_and(|active| active.id == lesson.id);
                grid.spawn((
                    Node {
                        flex_basis: px(265.),
                        min_width: px(200.),
                        flex_grow: 1.0,
                        padding: UiRect::all(px(11.)),
                        row_gap: px(5.),
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Start,
                        border: UiRect::all(px(1.)),
                        ..default()
                    },
                    BackgroundColor(INSET),
                    BorderColor::all(if current { MINT } else { EDGE }),
                ))
                .with_children(|card| {
                    card.spawn(text(format!("{}  /  {}", index + 1, lesson.name), 15., INK));
                    card.spawn(text(lesson.goal, 12., MUTED));
                    if let Some(saved) = saved {
                        let medal = saved.medal(lesson);
                        let detail = saved
                            .best
                            .map_or_else(String::new, |best| format!("  /  {}", format_time(best)));
                        card.spawn(text(
                            format!("{} medal{detail}", medal_label(medal)),
                            12.,
                            medal_color(medal),
                        ));
                    }
                    if current || saved.is_some() {
                        card.spawn(Node {
                            column_gap: px(6.),
                            row_gap: px(4.),
                            flex_wrap: FlexWrap::Wrap,
                            ..default()
                        })
                        .with_children(|buttons| {
                            button(
                                buttons,
                                if in_progress {
                                    "Resume lesson"
                                } else if current {
                                    "Start lesson"
                                } else {
                                    "Replay lesson"
                                },
                                Action::Request(MenuRequest::StartLesson(lesson.id)),
                                current,
                            );
                            button(
                                buttons,
                                "Watch demo",
                                Action::Request(MenuRequest::WatchLesson(lesson.id)),
                                false,
                            );
                        });
                    } else {
                        card.spawn(text("Complete the previous lesson to unlock", 11., MUTED));
                    }
                });
            }
        });
}

pub(super) fn finish(
    parent: &mut ChildSpawnerCommands,
    menu: &MenuState,
    lesson: &'static tutorial::Lesson,
    seconds: f64,
) {
    let saved = menu.training.completed.get(lesson.id);
    let next = menu.training.next();
    parent
        .spawn((
            Node {
                width: percent(100),
                max_width: px(620.),
                padding: UiRect::all(px(22.)),
                border: UiRect::all(px(1.)),
                row_gap: px(10.),
                align_items: AlignItems::Center,
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(PANEL),
            BorderColor::all(EDGE),
        ))
        .with_children(|card| {
            card.spawn(text("Lesson complete", 18., MINT));
            card.spawn(text(lesson.name, 30., INK));
            card.spawn(text(format_time(seconds), 40., INK));
            if let Some(result) = &menu.lesson_result {
                let medal = medal_label(result.medal);
                card.spawn(text(
                    if result.personal_best {
                        format!("{medal} medal  /  Personal best")
                    } else {
                        format!("{medal} medal")
                    },
                    16.,
                    medal_color(result.medal),
                ));
                if result.xp > 0 {
                    card.spawn(text(format!("+{} XP", result.xp), 17., GOLD));
                }
                render_sections(card, lesson, &result.sections, &menu.settings);
            }
            if !menu.notice.is_empty() {
                card.spawn((text(&menu.notice, 13., MUTED), Label::Notice));
            }
            if let Some(saved) = saved {
                let target = match saved.medal(lesson) {
                    Medal::Bronze => Some(("Silver", lesson.silver_seconds)),
                    Medal::Silver => Some(("Gold", lesson.gold_seconds)),
                    Medal::Gold => None,
                };
                if let Some((name, target)) = target {
                    card.spawn(text(
                        format!("{name} target  /  {}", format_time(target)),
                        13.,
                        MUTED,
                    ));
                }
            }
            if next.is_none() {
                card.spawn(text("Surf School complete", 16., GOLD));
            }
            card.spawn(Node {
                column_gap: px(8.),
                row_gap: px(8.),
                justify_content: JustifyContent::Center,
                flex_wrap: FlexWrap::Wrap,
                ..default()
            })
            .with_children(|buttons| {
                button(buttons, "Surf School", Action::Open(Page::Training), false);
                button(buttons, "Try again", Action::Restart, false);
                button(
                    buttons,
                    "Watch demo",
                    Action::Request(MenuRequest::WatchLesson(lesson.id)),
                    false,
                );
                if let Some(next) = next.filter(|next| saved.is_some() && next.id != lesson.id) {
                    button(
                        buttons,
                        "Next lesson",
                        Action::Request(MenuRequest::StartLesson(next.id)),
                        true,
                    );
                } else if next.is_none() {
                    button(buttons, "Choose a map", Action::Open(Page::Maps), false);
                    if let Some(step) =
                        Journey::new(&menu.training, &menu.records).next_path_course()
                    {
                        path_button(buttons, step);
                    }
                }
            });
        });
}

fn render_sections(
    card: &mut ChildSpawnerCommands,
    lesson: &tutorial::Lesson,
    sections: &[SectionResult],
    settings: &Settings,
) {
    let Some(first) = sections.first() else {
        return;
    };
    let comparing = first.delta.is_some();
    let recommended = section_to_practice(sections);
    card.spawn(Node {
        width: percent(100.),
        flex_direction: FlexDirection::Column,
        row_gap: px(4.),
        ..default()
    })
    .with_children(|details| {
        details.spawn(text(
            if comparing {
                "Section times · vs last clear"
            } else {
                "Section times"
            },
            12.,
            MUTED,
        ));
        for (index, (section, result)) in lesson.sections.iter().zip(sections).enumerate() {
            details
                .spawn(Node {
                    column_gap: px(10.),
                    row_gap: px(2.),
                    flex_wrap: FlexWrap::Wrap,
                    align_items: AlignItems::Center,
                    ..default()
                })
                .with_children(|row| {
                    row.spawn((
                        text(section.name, 13., INK),
                        Node {
                            flex_grow: 1.,
                            ..default()
                        },
                    ));
                    row.spawn(text(format_time(result.seconds), 13., INK));
                    if let Some(delta) = result.delta {
                        let color = if delta.abs() < 0.0005 {
                            MUTED
                        } else if delta > 0.0 {
                            GOLD
                        } else {
                            MINT
                        };
                        row.spawn((
                            text(section_delta(delta), 13., color),
                            Node {
                                min_width: px(65.),
                                ..default()
                            },
                        ));
                    }
                    if let Some(speed) = result.speed {
                        let mut label = format!("{speed:.0} units/s");
                        if let Some(delta) = result.speed_delta.filter(|delta| delta.abs() >= 0.5) {
                            label.push_str(&format!(" ({delta:+.0})"));
                        }
                        row.spawn(text(label, 12., MUTED));
                    }
                    button(
                        row,
                        "Practice",
                        Action::PracticeLessonSection(index),
                        recommended == Some(index),
                    );
                });
        }
        if let Some(section) = recommended.map(|index| &lesson.sections[index]) {
            details.spawn(text(
                format!(
                    "Practice {} · {}",
                    section.name.to_lowercase(),
                    menu_hud::lesson_bindings(section.tip, settings)
                ),
                13.,
                GOLD,
            ));
        }
    });
}

pub(super) fn drill_finish(
    parent: &mut ChildSpawnerCommands,
    menu: &MenuState,
    section: &tutorial::LessonSection,
    result: DrillResult,
    speed: f32,
    return_page: Page,
) {
    parent
        .spawn((
            Node {
                width: percent(100),
                max_width: px(520.),
                padding: UiRect::all(px(20.)),
                border: UiRect::all(px(1.)),
                row_gap: px(10.),
                align_items: AlignItems::Start,
                flex_direction: FlexDirection::Column,
                ..default()
            },
            BackgroundColor(PANEL),
            BorderColor::all(EDGE),
        ))
        .with_children(|card| {
            card.spawn(text("Section practice", 14., MINT));
            card.spawn(text(section.name, 26., INK));
            let base_tip = || menu_hud::lesson_bindings(section.tip, &menu.settings);
            let tip = match result {
                DrillResult::Ready => {
                    card.spawn(text(
                        format!("Start with the demo’s entry speed · {speed:.0} units/s"),
                        13.,
                        MUTED,
                    ));
                    base_tip()
                }
                DrillResult::Missed(feedback) => {
                    card.spawn(text("Section missed", 16., GOLD));
                    drill_miss_feedback(card, feedback, &menu.settings).unwrap_or_else(base_tip)
                }
                DrillResult::Cleared(seconds) => {
                    card.spawn(text("Section cleared", 16., MINT));
                    card.spawn(text(format_time(seconds), 34., INK));
                    base_tip()
                }
            };
            card.spawn(text(tip, 14., INK));
            card.spawn(Node {
                column_gap: px(8.),
                row_gap: px(6.),
                flex_wrap: FlexWrap::Wrap,
                ..default()
            })
            .with_children(|buttons| {
                button(
                    buttons,
                    if result == DrillResult::Ready {
                        "Start practice"
                    } else {
                        "Practice again"
                    },
                    Action::Restart,
                    true,
                );
                button(
                    buttons,
                    drill_return_label(return_page),
                    Action::ExitLessonDrill,
                    false,
                );
            });
        });
}

fn drill_miss_feedback(
    card: &mut ChildSpawnerCommands,
    feedback: DrillFeedback,
    settings: &Settings,
) -> Option<String> {
    card.spawn(text(
        format!("Peak horizontal speed · {:.0} units/s", feedback.peak_speed),
        13.,
        MUTED,
    ));
    let (measurement, tip) = match feedback.finding? {
        DrillFinding::PadHop {
            milliseconds,
            jumped,
        } => {
            let measurement = if jumped {
                format!("Jump input came {milliseconds} ms after you landed on the violet pad.")
            } else {
                format!("You spent {milliseconds} ms on the violet pad without jumping.")
            };
            (measurement, menu_hud::pad_jump_tip(settings))
        }
        DrillFinding::AirTurn {
            right_degrees,
            paired_degrees,
            strafe_ms,
        } if right_degrees - paired_degrees < 8.0 => (
            format!("Right strafe · {strafe_ms} ms  /  Rightward turn · {right_degrees:.0}°"),
            menu_hud::lesson_bindings(
                "Turn the mouse right as you hold {right}. Aim for about 40° toward the gold deck.",
                settings,
            ),
        ),
        DrillFinding::AirTurn {
            right_degrees,
            paired_degrees,
            ..
        } => (
            format!(
                "Rightward turn while strafing right · {paired_degrees:.0}° of {right_degrees:.0}°"
            ),
            menu_hud::lesson_bindings(
                "Hold {right} through the turn toward the gold deck.",
                settings,
            ),
        ),
    };
    card.spawn(text(measurement, 13., MINT));
    Some(tip)
}

pub(super) fn drill_return_label(page: Page) -> &'static str {
    if page == Page::Pause {
        "Return to paused lesson"
    } else {
        "Return to lesson results"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_surf::{course::Course, movement::MoveInput, session::Session};

    #[test]
    fn completion_survives_reopen_and_first_clears_do_not_award_twice() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let data =
            std::env::temp_dir().join(format!("surf-school-test-{}-{unique}", std::process::id()));
        let first = &tutorial::LESSONS[0];
        let second = &tutorial::LESSONS[1];
        let training = TrainingProgress::read(&data);
        assert!(training.unlocked(first.id));
        assert!(!training.unlocked(second.id));
        fs::create_dir_all(&training.directory).unwrap();
        fs::File::create_new(training.directory.join(first.id)).unwrap();

        let mut training = TrainingProgress::read(&data);
        assert!(training.complete_lesson(first.id));
        assert_eq!(training.next().map(|lesson| lesson.id), Some(second.id));
        assert_eq!(training.best(first.id), None);
        assert_eq!(training.xp(), medal_xp(Medal::Bronze));

        let bronze = training
            .record(first, first.silver_seconds + 1., &[], &[])
            .unwrap();
        assert_eq!(bronze.medal, Medal::Bronze);
        assert_eq!(bronze.xp, 0);
        let mut second_client = TrainingProgress::read(&data);
        let silver = training
            .record(first, first.silver_seconds, &[], &[])
            .unwrap();
        assert_eq!(silver.medal, Medal::Silver);
        assert_eq!(silver.xp, 30);
        let gold = training
            .record(first, first.gold_seconds, &[], &[])
            .unwrap();
        assert_eq!(gold.medal, Medal::Gold);
        assert_eq!(gold.xp, 50);
        let slower = second_client
            .record(first, first.silver_seconds, &[], &[])
            .unwrap();
        assert_eq!(slower.xp, 0);
        assert!(!slower.personal_best);

        fs::write(
            training.directory.join(first.id),
            first.gold_seconds.to_string(),
        )
        .unwrap();
        let training = TrainingProgress::read(&data);
        assert_eq!(training.best(first.id), Some(first.gold_seconds));
        assert!(training.unlocked(second.id));
        let entry = catalog::find("kitsune").unwrap();
        let records = [
            entry.id.clone(),
            entry.slug.clone(),
            format!("{} · Manual bhop", entry.id),
        ]
        .into_iter()
        .map(|name| {
            (
                name,
                BestRun {
                    seconds: 20.0,
                    splits: vec![],
                    speeds: vec![],
                },
            )
        })
        .collect();
        let journey = Journey::new(&training, &records);
        assert_eq!(journey.courses.len(), 1);
        assert_eq!(journey.xp, COURSE_XP + medal_xp(Medal::Gold));
        assert_eq!(journey.path_clears(), 1);
        assert_eq!(journey.next_path_course().unwrap().id, "surf_tendies");

        for id in [
            "tutorial-catch",
            "tutorial-strafe",
            "tutorial-transfer",
            "tutorial-rise",
            "tutorial-line",
        ] {
            fs::File::create_new(training.directory.join(id)).unwrap();
        }
        let mut training = TrainingProgress::read(&data);
        let final_lesson = training.next().unwrap();
        assert_eq!(final_lesson.id, "tutorial-bhop");
        assert_eq!(training.completed.len(), 6);
        assert_eq!(
            training.xp(),
            medal_xp(Medal::Gold) + 5 * medal_xp(Medal::Bronze)
        );
        assert!(training.unlocked(final_lesson.id));
        let course = tutorial::course(final_lesson.id).unwrap();
        let early = clear_bhop(&course, -196.0);
        let first_clear = training
            .record(
                final_lesson,
                early.elapsed,
                &early.splits,
                &early.checkpoint_speeds,
            )
            .unwrap();
        assert_eq!(first_clear.sections.len(), 3);
        assert!(
            first_clear
                .sections
                .iter()
                .all(|section| section.delta.is_none())
        );
        assert_eq!(section_to_practice(&first_clear.sections), None);

        // An invalid split must not replace the last usable comparison.
        let malformed = training
            .record(
                final_lesson,
                early.elapsed + 0.1,
                &[f64::NAN, 1.0],
                &early.checkpoint_speeds,
            )
            .unwrap();
        assert!(malformed.sections.is_empty());
        let mut training = TrainingProgress::read(&data);
        let late = clear_bhop(&course, -246.0);
        let second_clear = training
            .record(
                final_lesson,
                late.elapsed,
                &late.splits,
                &late.checkpoint_speeds,
            )
            .unwrap();
        let first_section = &second_clear.sections[0];
        let first_section_loss = late.splits[0] - early.splits[0];
        assert!(first_section_loss > 0.1);
        assert!((first_section.delta.unwrap() - first_section_loss).abs() < 1e-12);
        assert_eq!(first_section.speed, Some(late.checkpoint_speeds[0]));
        assert_eq!(section_to_practice(&second_clear.sections), Some(0));
        assert_eq!(second_clear.xp, 0);

        let mut settings = Settings::default();
        settings.set_binding(ControlAction::Jump, Some(0), Binding::key(KeyCode::KeyX));
        let mut app = App::new();
        app.add_systems(Startup, move |mut commands: Commands| {
            commands.spawn(Node::default()).with_children(|card| {
                render_sections(card, final_lesson, &second_clear.sections, &settings);
            });
        });
        app.update();
        let world = app.world_mut();
        let mut labels = world.query::<&Text>();
        assert!(labels.iter(world).any(|text| text.0 == "+0.195s"));
        assert!(labels.iter(world).any(|text| {
            text.0 == "Practice first jump · Build speed before the cyan stripe. Jump with X as you reach it."
        }));

        let training = TrainingProgress::read(&data);
        assert!(training.next().is_none());
        assert_eq!(
            training.best(final_lesson.id),
            Some(early.elapsed.min(late.elapsed))
        );
        assert_eq!(
            training.completed[final_lesson.id]
                .last
                .as_ref()
                .unwrap()
                .splits,
            late.splits
        );
        for id in training.completed.keys() {
            fs::remove_file(training.directory.join(id)).unwrap();
        }
        fs::remove_file(training.directory.join(".lock")).unwrap();
        fs::remove_dir(training.directory).unwrap();
        fs::remove_dir(data).unwrap();
    }

    fn clear_bhop(course: &Course, launch: f32) -> Session {
        let mut session = Session::new(course);
        for _ in 0..400 {
            let z = session.player.position.z;
            session.tick(
                course,
                MoveInput {
                    forward: 1.0,
                    jump: z <= launch && z > -748.0,
                    ..Default::default()
                },
            );
            if session.phase == RunPhase::Finished {
                return session;
            }
        }
        panic!("bunnyhop lesson did not finish from launch at {launch}");
    }
}
