use bevy_surf::{
    catalog,
    record_protocol::{Board, BoardRow},
};

use super::*;
use crate::online::Action as OnlineAction;

const SURFACE: Color = Color::srgba_u8(36, 50, 58, 242);
const HEADER: Color = Color::srgb_u8(36, 50, 58);
const LIGHT_EDGE: Color = Color::srgb_u8(109, 137, 149);
const DARK_EDGE: Color = Color::srgb_u8(8, 16, 20);
const QUIET: Color = Color::srgb_u8(152, 174, 184);
const FIRST: Color = Color::srgb_u8(235, 203, 143);
const OWN: Color = Color::srgb_u8(35, 65, 76);
const STRIPE: Color = Color::srgba_u8(255, 255, 255, 6);
const TOP_RUNS: usize = 10;
const BOARD_LIMIT: usize = 50;

#[derive(Clone, Copy, PartialEq)]
pub(super) struct Layout {
    pub left_width: f32,
    width: f32,
    right: f32,
    top: f32,
    max_height: f32,
    row_height: f32,
}

impl Layout {
    pub fn new(width: f32, height: f32) -> Option<Self> {
        if width <= 760. || height < 1. {
            return None;
        }
        let right = (width * 0.03).clamp(18., 56.);
        let board_width = if width <= 1000. {
            (width * 0.40).min(360.)
        } else {
            (width * 0.34).min(420.)
        };
        let top = (height * 0.036).clamp(18., 42.) + 44.;
        Some(Self {
            left_width: (width * 0.95 - right - board_width - 20.).min(520.),
            width: board_width,
            right,
            top,
            max_height: (height - top - 20.).max(0.),
            row_height: if height <= 700. { 24. } else { 26. },
        })
    }
}

pub(super) struct Icons {
    watch: Handle<Image>,
    race: Handle<Image>,
}

impl Icons {
    pub fn new(images: &mut Assets<Image>) -> Self {
        let mut image = |mask: [&str; 12]| {
            let pixels = mask
                .into_iter()
                .flat_map(|row| {
                    row.bytes()
                        .flat_map(|pixel| [165, 216, 219, if pixel == b'#' { 255 } else { 0 }])
                })
                .collect();
            images.add(Image::new(
                Extent3d {
                    width: 12,
                    height: 12,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                pixels,
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            ))
        };
        Self {
            watch: image([
                "............",
                "...#........",
                "...###......",
                "...####.....",
                "...######...",
                "...#######..",
                "...#######..",
                "...######...",
                "...####.....",
                "...###......",
                "...#........",
                "............",
            ]),
            race: image([
                "............",
                "....####....",
                "..########..",
                ".##########.",
                ".##..##..##.",
                ".##..##..##.",
                ".##########.",
                ".##########.",
                ".##########.",
                ".###.##.###.",
                ".##......##.",
                "............",
            ]),
        }
    }
}

pub(super) fn render(
    parent: &mut ChildSpawnerCommands,
    menu: &MenuState,
    game: &Game,
    online: &Online,
    layout: Layout,
    icons: &Icons,
) {
    let board = online.pause_board();
    let bonus = game.selected_bonus();
    let bonus_label = bonus.map(|number| format!("Bonus {number}"));
    let title = catalog::find(&game.course.name)
        .map_or(game.course.name.as_str(), |entry| entry.name.as_str());
    let loading_ghost = game.ghost_preparation_progress().is_some() || online.pause_replay_busy();
    let ghost_active = game.ghost_enabled();
    let status = online.pause_board_status();
    let ghost_label = if loading_ghost {
        if status.starts_with("Loading replay") {
            "Loading replay…"
        } else {
            "Loading ghost…"
        }
    } else if ghost_active {
        "Record Ghost on"
    } else {
        "Race the Record Ghost"
    };
    let owner = online.profile.as_ref().map(|profile| profile.id);
    let subtitle = board.filter(|board| !board.rows.is_empty()).map(|board| {
        let count = board.rows.len();
        if count >= BOARD_LIMIT {
            format!("Top {TOP_RUNS}")
        } else {
            format!("Top {} of {count}", count.min(TOP_RUNS))
        }
    });
    let detail = match (bonus_label.as_deref(), subtitle.as_deref()) {
        (Some(bonus), Some(top)) => format!("{bonus} · {top}"),
        (Some(value), None) | (None, Some(value)) => value.to_owned(),
        (None, None) => String::new(),
    };
    parent
        .spawn((
            Name::new("Pause leaderboard"),
            Node {
                position_type: PositionType::Absolute,
                top: px(layout.top),
                right: px(layout.right),
                width: px(layout.width),
                max_height: px(layout.max_height),
                flex_direction: FlexDirection::Column,
                border: UiRect::all(px(1.)),
                ..default()
            },
            BackgroundColor(SURFACE),
            BorderColor {
                top: LIGHT_EDGE,
                left: LIGHT_EDGE,
                right: DARK_EDGE,
                bottom: DARK_EDGE,
            },
        ))
        .with_children(|panel| {
            panel
                .spawn((
                    Node {
                        padding: UiRect {
                            left: px(12),
                            right: px(10),
                            top: px(8),
                            bottom: px(8),
                        },
                        align_items: AlignItems::Center,
                        column_gap: px(10),
                        flex_shrink: 0.,
                        border: UiRect::bottom(px(1)),
                        ..default()
                    },
                    BackgroundColor(HEADER),
                    BorderColor::all(DARK_EDGE),
                ))
                .with_children(|header| {
                    header
                        .spawn(Node {
                            flex_grow: 1.,
                            min_width: px(0),
                            overflow: Overflow::clip(),
                            align_items: AlignItems::Center,
                            column_gap: px(6),
                            ..default()
                        })
                        .with_children(|heading| {
                            heading.spawn((text(title, 13., INK), TextLayout::no_wrap()));
                            if !detail.is_empty() {
                                heading.spawn((text(detail, 11., QUIET), TextLayout::no_wrap()));
                            }
                        });
                    if loading_ghost {
                        header.spawn(text(ghost_label, 11., QUIET));
                    } else {
                        link(
                            header,
                            ghost_label,
                            Action::Request(MenuRequest::PauseRecordGhost),
                        );
                    }
                });

            panel
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        min_height: px(0),
                        overflow: Overflow::scroll_y(),
                        padding: UiRect::vertical(px(4)),
                        ..default()
                    },
                    MenuScroll,
                    ScrollPosition::default(),
                    RelativeCursorPosition::default(),
                ))
                .with_children(|list| {
                    if let Some(board) = board.filter(|board| !board.rows.is_empty()) {
                        for (index, record) in featured_rows(board, owner) {
                            if index >= TOP_RUNS {
                                list.spawn((
                                    Node {
                                        height: px(1),
                                        flex_shrink: 0.,
                                        margin: UiRect::axes(px(12), px(3)),
                                        ..default()
                                    },
                                    BackgroundColor(EDGE),
                                ));
                            }
                            spawn_row(
                                list,
                                online,
                                icons,
                                record,
                                index,
                                layout.row_height,
                                owner,
                                bonus.is_none(),
                            );
                        }
                    } else {
                        let message = if !status.is_empty() {
                            status
                        } else if board.is_some() {
                            "No times yet. The first one is yours."
                        } else {
                            "Loading the board…"
                        };
                        list.spawn((
                            Node {
                                padding: UiRect::axes(px(14), px(14)),
                                ..default()
                            },
                            text(message, 12., QUIET),
                        ));
                        if let Some(best) = menu.records.get(&menu.record_key(game)) {
                            let time =
                                online_ui::surfd_time((best.seconds * 1_000_000.).round() as u64);
                            list.spawn((
                                Node {
                                    padding: UiRect::axes(px(14), px(4)),
                                    ..default()
                                },
                                text(format!("Your local best  {time}"), 12., MINT),
                            ));
                        }
                    }
                });

            if board.is_some_and(|board| !board.rows.is_empty()) && !status.is_empty() {
                panel.spawn((
                    Node {
                        padding: UiRect::axes(px(12), px(4)),
                        flex_shrink: 0.,
                        ..default()
                    },
                    text(status, 11., QUIET),
                ));
            }

            panel
                .spawn((
                    Node {
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::Center,
                        padding: UiRect::axes(px(9), px(5)),
                        column_gap: px(7),
                        border: UiRect::top(px(1)),
                        flex_shrink: 0.,
                        ..default()
                    },
                    BackgroundColor(HEADER),
                    BorderColor::all(DARK_EDGE),
                ))
                .with_children(|footer| {
                    let watch = bonus
                        .and_then(|number| {
                            game.course
                                .bonus_routes
                                .iter()
                                .find(|route| route.number == number)
                        })
                        .map_or_else(
                            || "Watch the record route".into(),
                            |route| format!("Watch the {} route", route.name),
                        );
                    link(
                        footer,
                        &watch,
                        Action::Request(MenuRequest::PauseWatchRecordRoute),
                    );
                    let mut full = footer.spawn((
                        Node {
                            padding: UiRect::all(px(3)),
                            flex_shrink: 0.,
                            ..default()
                        },
                        HoverSurface {
                            idle: Color::NONE,
                            hover: TILE,
                        },
                        BackgroundColor(Color::NONE),
                    ));
                    online_ui::bind_action(&mut full, OnlineAction::PauseFullBoard);
                    full.observe(hover_in)
                        .observe(hover_out)
                        .with_child((text("Full leaderboard", 12., MINT), Pickable::IGNORE));
                });
        });
}

fn featured_rows(board: &Board, owner: Option<i64>) -> impl Iterator<Item = (usize, &BoardRow)> {
    board.rows.iter().enumerate().filter(move |(index, row)| {
        *index < TOP_RUNS || owner.is_some_and(|owner| row.account_id == Some(owner))
    })
}

fn spawn_row(
    list: &mut ChildSpawnerCommands,
    online: &Online,
    icons: &Icons,
    record: &BoardRow,
    index: usize,
    height: f32,
    owner: Option<i64>,
    race_available: bool,
) {
    let own = owner.is_some_and(|owner| record.account_id == Some(owner));
    let racing = online.pause_ghost_id() == Some(record.id.as_str());
    let fill = if own {
        OWN
    } else if index % 2 != 0 {
        STRIPE
    } else {
        Color::NONE
    };
    list.spawn((
        Node {
            min_height: px(height),
            flex_shrink: 0.,
            padding: UiRect {
                left: px(11),
                right: px(10),
                top: px(1),
                bottom: px(1),
            },
            column_gap: px(8),
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(fill),
        HoverSurface {
            idle: fill,
            hover: TILE,
        },
    ))
    .observe(hover_in)
    .observe(hover_out)
    .with_children(|row| {
        if racing {
            row.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(0),
                    top: px(0),
                    bottom: px(0),
                    width: px(2),
                    ..default()
                },
                BackgroundColor(MINT),
                Pickable::IGNORE,
            ));
        }
        row.spawn((
            Node {
                width: px(26),
                flex_shrink: 0.,
                ..default()
            },
            text(
                (index + 1).to_string(),
                12.,
                if index == 0 { FIRST } else { QUIET },
            ),
            Pickable::IGNORE,
        ));
        let mut player = row.spawn(Node {
            flex_grow: 1.,
            min_width: px(0),
            column_gap: px(6),
            align_items: AlignItems::Center,
            overflow: Overflow::clip(),
            ..default()
        });
        if let Some(id) = record.account_id {
            online_ui::bind_action(&mut player, OnlineAction::ViewProfile(id));
        }
        player.with_children(|name| {
            name.spawn((
                Node {
                    width: px(18),
                    height: px(18),
                    flex_shrink: 0.,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                BackgroundColor(ACTIVE),
                Pickable::IGNORE,
            ))
            .with_child((
                text(
                    record
                        .name
                        .chars()
                        .next()
                        .unwrap_or('?')
                        .to_uppercase()
                        .to_string(),
                    10.,
                    MINT,
                ),
                Pickable::IGNORE,
            ));
            name.spawn((
                text(&record.name, 12., if own { MINT } else { INK }),
                TextLayout::no_wrap(),
                Pickable::IGNORE,
            ));
            if online
                .social
                .friends
                .iter()
                .any(|friend| record.account_id == Some(friend.id))
            {
                name.spawn((
                    text("Friend", 10., MINT),
                    TextLayout::no_wrap(),
                    Pickable::IGNORE,
                ));
            }
        });
        row.spawn((
            Node {
                width: px(76),
                flex_shrink: 0.,
                ..default()
            },
            text(online_ui::surfd_time(record.time_us), 12., INK),
            TextLayout::no_wrap().with_justify(Justify::Right),
            Pickable::IGNORE,
        ));
        row.spawn(Node {
            width: px(56),
            flex_shrink: 0.,
            justify_content: JustifyContent::FlexEnd,
            column_gap: px(3),
            ..default()
        })
        .with_children(|actions| {
            if record.id.is_empty() || record.time_us > 600_000_000 {
                return;
            }
            if race_available {
                icon_button(
                    actions,
                    "Race ghost",
                    &icons.race,
                    online.pause_replay_action(&record.id, true),
                    racing,
                );
            }
            icon_button(
                actions,
                "Watch replay",
                &icons.watch,
                online.pause_replay_action(&record.id, false),
                false,
            );
        });
    });
}

fn icon_button(
    parent: &mut ChildSpawnerCommands,
    title: &'static str,
    icon: &Handle<Image>,
    action: OnlineAction,
    active: bool,
) {
    let idle = if active { ACTIVE } else { TILE };
    let mut button = parent.spawn((
        Name::new(title),
        Node {
            width: px(25),
            height: px(21),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border: UiRect::all(px(1)),
            ..default()
        },
        BackgroundColor(idle),
        BorderColor::all(if active { MINT } else { EDGE }),
        HoverSurface {
            idle,
            hover: ACTIVE,
        },
    ));
    online_ui::bind_action(&mut button, action);
    button.observe(hover_in).observe(hover_out).with_child((
        ImageNode::new(icon.clone()),
        Node {
            width: px(12),
            height: px(12),
            ..default()
        },
        Pickable::IGNORE,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_board_preserves_the_web_breakpoint_and_keeps_both_columns_apart() {
        assert!(Layout::new(760., 700.).is_none());
        let narrow = Layout::new(800., 700.).unwrap();
        assert_eq!(narrow.width, 320.);
        assert_eq!(narrow.row_height, 24.);
        assert!(40. + narrow.left_width < 800. - narrow.right - narrow.width);
        let wide = Layout::new(1280., 800.).unwrap();
        assert_eq!(
            (wide.width, wide.left_width, wide.row_height),
            (420., 520., 26.)
        );
    }

    #[test]
    fn pause_board_keeps_a_personal_best_below_the_top_ten() {
        let board = Board {
            map: "surf_kitsune".into(),
            bonus: None,
            stage: None,
            rows: (1..=12)
                .map(|account| BoardRow {
                    id: account.to_string(),
                    name: account.to_string(),
                    account_id: Some(account),
                    time_us: 0,
                    splits_us: vec![],
                    checkpoint_speeds: vec![],
                    stage_span: None,
                })
                .collect(),
        };
        assert_eq!(featured_rows(&board, None).count(), 10);
        assert_eq!(featured_rows(&board, Some(5)).count(), 10);
        assert_eq!(
            featured_rows(&board, Some(12))
                .last()
                .map(|(index, _)| index),
            Some(11)
        );
    }
}
