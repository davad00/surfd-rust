use std::time::Instant;

use bevy::{
    ecs::hierarchy::ChildSpawnerCommands,
    math::Rot2,
    picking::{Pickable, events::PointerClick, pointer::PointerButton},
    prelude::*,
    ui::UiTransform,
};

use crate::{
    menu::Label,
    online::{
        Action, Field, NUMBERED_VOTE_CHOICES, Online, OnlinePage, RoomAvailability, RunSplits,
        SURFD_HISTORY_PAGE_SIZE,
    },
};
use bevy_surf::{
    catalog::{self, MapMode},
    network::{
        CONTINUE_MAP, EXTEND_MAP, ROOM_REPORT_PAGE_SIZE, RoomModeration, VoteInfo, VoteKind,
        map_time_label,
    },
    record_protocol::{OAuthProvider, PLAYER_RUNS_PAGE_SIZE, PlayerRunSort, Playtime, Profile},
    social_protocol::{
        DirectMessage, MessageReportPage, MessageReportReason, NotificationKind, PlayerActivity,
        SocialAction,
    },
};

const TEXT: Color = Color::srgb(0.93, 0.96, 0.96);
const MUTED: Color = Color::srgb(0.62, 0.72, 0.74);
const ACCENT: Color = Color::srgb(0.52, 0.96, 0.73);
const PANEL: Color = Color::srgba(0.08, 0.11, 0.13, 0.93);
const BUTTON: Color = Color::srgb(0.13, 0.19, 0.21);
const EDGE: Color = Color::srgb(0.21, 0.29, 0.31);
const VOTE_SELECTED: Color = Color::srgb(0.15, 0.27, 0.23);
const VOTE_KEY_BACKGROUND: Color = Color::srgb(0.08, 0.11, 0.13);
const VOTE_SHARE: Color = Color::srgba(0.52, 0.96, 0.73, 0.5);

#[derive(Component)]
struct OnlineButton(Action);

pub(crate) fn bind_action(entity: &mut EntityCommands, action: Action) {
    entity.insert(OnlineButton(action)).observe(click);
}

#[derive(Component)]
pub(crate) struct VotePanel;

pub fn panel(
    parent: &mut ChildSpawnerCommands,
    page: OnlinePage,
    online: &Online,
    course_name: &str,
) {
    match page {
        OnlinePage::Servers => servers(parent, online),
        OnlinePage::Players => players(parent, online),
        OnlinePage::Records => records(parent, online, course_name),
        OnlinePage::Profile => profile(parent, online),
        OnlinePage::Friends => friends(parent, online),
        OnlinePage::Moderation => moderation(parent, online),
    }
}

fn servers(parent: &mut ChildSpawnerCommands, online: &Online) {
    heading(parent, "Find a server", &online.status);
    horizontal(parent, |row| {
        field(row, online, Field::Address, "Server address", 275.);
        field(row, online, Field::Name, "Player name", 155.);
        button(row, "Connect", Action::Join, true);
        button(row, "Refresh servers", Action::Discover, false);
    });
    if let Some(room) = &online.room {
        horizontal(parent, |row| {
            row.spawn(label(
                format!(
                    "Connected to {} · {} · {}",
                    room.name,
                    room.mode.label(),
                    map_title(&room.map)
                ),
                16.,
                ACCENT,
            ));
            button(row, "Leave server", Action::Leave, false);
        });
    }
    if online.servers.is_empty() {
        parent.spawn(label(
            if online.discovering_servers() {
                "Checking configured servers…"
            } else {
                "No configured servers found. Enter a server address to connect."
            },
            15.,
            MUTED,
        ));
    }
    for mode in [catalog::MapMode::Surf, catalog::MapMode::Bhop] {
        let mut rooms = online
            .servers
            .iter()
            .filter_map(|server| match &server.availability {
                RoomAvailability::Online { info, ping_ms } if info.mode == mode => {
                    Some((server, info, ping_ms))
                }
                _ => None,
            })
            .peekable();
        if rooms.peek().is_some() {
            parent.spawn(label(format!("{} rooms", mode.label()), 16., ACCENT));
        }
        for (server, info, ping_ms) in rooms {
            let mut title = format!(
                "{}   ·   {}   ·   {}/{} players   ·   {} ms",
                info.name,
                map_title(&info.map),
                info.players,
                info.capacity,
                ping_ms
            );
            if let Some(ms) = info.map_remaining_ms {
                title.push_str("   ·   ");
                title.push_str(&map_time_label(std::time::Duration::from_millis(ms)));
            }
            line(parent, |row| {
                flexible(row, title, 16.);
                button(
                    row,
                    "Join",
                    Action::JoinAddress(server.address.clone()),
                    true,
                );
            });
        }
    }
    for server in &online.servers {
        if let RoomAvailability::Unavailable { last_name } = &server.availability {
            let title = last_name.as_ref().map_or_else(
                || format!("{}   ·   Unavailable", server.address),
                |name| format!("{name}   ·   {}   ·   Unavailable", server.address),
            );
            line(parent, |row| flexible(row, title, 16.));
        }
    }
}

fn players(parent: &mut ChildSpawnerCommands, online: &Online) {
    let Some(room) = &online.room else {
        heading(
            parent,
            "Players",
            "Join a server to see its roster and chat.",
        );
        return;
    };
    heading(
        parent,
        &room.name,
        &format!(
            "{} · {} · {}/{} players",
            room.mode.label(),
            map_title(&room.map),
            online.players.len(),
            room.capacity
        ),
    );
    if online.room_map_time(std::time::Instant::now()).is_some() {
        parent.spawn((label("", 14., MUTED), Label::RoomMapClock));
    }
    horizontal(parent, |row| {
        button(row, "Return to play", Action::Spectate(None), false);
        if !online.managed_guest() {
            button(row, "RTV", Action::RockTheVote, false);
            field(row, online, Field::Nomination, "Map to nominate", 210.);
            button(row, "Nominate", Action::Nominate, false);
            if online
                .room_identity()
                .is_some_and(|identity| identity.points > 1000)
                && online.vote.as_ref().is_none_or(|vote| !vote.open)
            {
                button(row, "Extend", Action::Extend, false);
            }
        }
        button(row, "Leave", Action::Leave, false);
    });
    vote_panel(parent, online, Instant::now(), 1., false);
    if room.room_id.is_none() && online.active_vote(Instant::now()).is_some() {
        button(parent, "Extend", Action::Extend, false);
    }
    if let Some(vote) = online
        .vote
        .as_ref()
        .filter(|vote| !vote.open && vote.rtv > 0)
    {
        parent.spawn(label(
            format!("RTV {}/{}", vote.rtv, vote.needed),
            14.,
            MUTED,
        ));
    }
    for player in online.players.iter().take(7) {
        line(parent, |row| {
            let suffix = if Some(player.id) == online.id {
                " (you)"
            } else if player.spectating.is_some() {
                " (spectating)"
            } else {
                ""
            };
            flexible(row, format!("{}{suffix}", player.name), 15.);
            if let Some(identity) = player.identity {
                if identity.moderator {
                    row.spawn(label("Moderator", 13., ACCENT));
                } else if identity.account_id.is_none() {
                    row.spawn(label("Guest", 13., MUTED));
                }
                if let Some(account) = identity.account_id {
                    row.spawn(label(
                        format!(
                            "{} · {} XP",
                            identity.rank.label_for(room.mode),
                            identity.points
                        ),
                        13.,
                        MUTED,
                    ));
                    button(row, "Profile", Action::ViewProfile(account), false);
                }
            }
            if Some(player.id) != online.id {
                button(row, "Spectate", Action::Spectate(Some(player.id)), false);
            }
        });
    }
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Column,
            row_gap: px(3),
            ..default()
        })
        .with_children(|chat| {
            for text in online.messages.iter().rev().take(6).rev() {
                chat.spawn(label(text, 14., TEXT));
            }
        });
    if online.managed_guest() {
        parent.spawn(label(
            "Sign in to chat, nominate maps, and vote.",
            14.,
            MUTED,
        ));
    }
    horizontal(parent, |row| {
        let hint = if online.managed_guest() {
            "Command or /help"
        } else {
            "Message or /help"
        };
        field(row, online, Field::Chat, hint, 420.);
        button(row, "Send", Action::Chat, true);
    });
}

pub(crate) fn vote_panel(
    parent: &mut ChildSpawnerCommands,
    online: &Online,
    now: Instant,
    scale: f32,
    in_game: bool,
) {
    let Some((vote, seconds)) = online
        .active_vote(now)
        .filter(|(vote, _)| !vote.maps.is_empty())
    else {
        return;
    };
    let choices = vote.maps.iter().take(NUMBERED_VOTE_CHOICES);
    let total = choices.clone().fold(0_usize, |sum, map| {
        sum.saturating_add(vote.votes.get(map).copied().unwrap_or_default())
    });
    let selected = online.selected_vote_choice(vote);
    let guest = online.managed_guest();
    // A live player's mouse is grabbed. Number keys stay available while moving.
    let can_click = !in_game || (online.spectating.is_some() && !online.is_typing());
    let mut panel = Node {
        width: px(316. * scale),
        max_width: percent(90),
        border: UiRect::all(px(1.)),
        padding: UiRect::all(px(5. * scale)),
        row_gap: px(4. * scale),
        flex_direction: FlexDirection::Column,
        ..default()
    };
    if in_game {
        panel.position_type = PositionType::Absolute;
        panel.top = px(120.);
        panel.left = px(22.);
    }
    parent
        .spawn((
            VotePanel,
            panel,
            BackgroundColor(PANEL),
            BorderColor::all(EDGE),
            Pickable::IGNORE,
        ))
        .with_children(|panel| {
            panel
                .spawn((
                    Node {
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::SpaceBetween,
                        column_gap: px(6. * scale),
                        padding: UiRect::axes(px(7. * scale), px(4. * scale)),
                        ..default()
                    },
                    BackgroundColor(BUTTON),
                    Pickable::IGNORE,
                ))
                .with_children(|header| {
                    header.spawn((label(vote_title(vote), 13. * scale, TEXT), Pickable::IGNORE));
                    header.spawn((
                        label(format!("{seconds}s"), 12. * scale, ACCENT),
                        Label::RoomVoteClock,
                        Pickable::IGNORE,
                    ));
                });
            for (index, map) in choices.enumerate() {
                let number = index as u8 + 1;
                let count = vote.votes.get(map).copied().unwrap_or_default();
                let chosen = selected == Some(number);
                let action = if can_click {
                    online.vote_action(number, now)
                } else {
                    None
                };
                let mut row = panel.spawn((
                    Node {
                        min_height: px(38. * scale),
                        padding: UiRect::all(px(7. * scale)),
                        border: UiRect::all(px(1.)),
                        align_items: AlignItems::Center,
                        column_gap: px(7. * scale),
                        ..default()
                    },
                    BackgroundColor(if chosen { VOTE_SELECTED } else { BUTTON }),
                    BorderColor::all(if chosen { ACCENT } else { EDGE }),
                ));
                if let Some(action) = action {
                    row.insert(OnlineButton(action)).observe(click);
                } else {
                    row.insert(Pickable::IGNORE);
                }
                row.with_children(|row| {
                    row.spawn((
                        Node {
                            padding: UiRect::axes(px(5. * scale), px(2. * scale)),
                            border: UiRect::all(px(1.)),
                            ..default()
                        },
                        BackgroundColor(VOTE_KEY_BACKGROUND),
                        BorderColor::all(EDGE),
                        Pickable::IGNORE,
                    ))
                    .with_child((
                        label(number.to_string(), 12. * scale, ACCENT),
                        Pickable::IGNORE,
                    ));
                    if let Some(tier) = catalog::find(map).and_then(|entry| entry.tier) {
                        row.spawn((
                            label(format!("T{tier}"), 10. * scale, ACCENT),
                            Pickable::IGNORE,
                        ));
                    }
                    row.spawn((
                        Node {
                            flex_grow: 1.,
                            min_width: px(0.),
                            ..default()
                        },
                        label(
                            vote_choice_title(map, vote.kind),
                            11. * scale,
                            if chosen { ACCENT } else { TEXT },
                        ),
                        Pickable::IGNORE,
                    ));
                    if chosen {
                        row.spawn((
                            Node {
                                width: px(4. * scale),
                                height: px(8. * scale),
                                border: UiRect {
                                    right: px(1.5 * scale),
                                    bottom: px(1.5 * scale),
                                    ..default()
                                },
                                ..default()
                            },
                            UiTransform::from_rotation(Rot2::radians(std::f32::consts::FRAC_PI_4)),
                            BorderColor::all(ACCENT),
                            Pickable::IGNORE,
                        ));
                    }
                    let votes = if count == 1 { "vote" } else { "votes" };
                    row.spawn((
                        label(
                            format!("{count} {votes}"),
                            10. * scale,
                            if chosen { ACCENT } else { MUTED },
                        ),
                        Pickable::IGNORE,
                    ));
                    if count > 0 {
                        row.spawn((
                            Node {
                                position_type: PositionType::Absolute,
                                bottom: px(0.),
                                left: px(0.),
                                width: percent(100. * (count as f32 / total as f32)),
                                height: px(2. * scale),
                                ..default()
                            },
                            BackgroundColor(VOTE_SHARE),
                            Pickable::IGNORE,
                        ));
                    }
                });
            }
            panel.spawn((
                Node {
                    margin: UiRect::axes(px(7. * scale), px(2. * scale)),
                    ..default()
                },
                label(
                    if guest {
                        "Sign in to vote."
                    } else {
                        "Press a number to vote. Keep surfing."
                    },
                    10. * scale,
                    MUTED,
                ),
                Pickable::IGNORE,
            ));
        });
}

fn vote_choice_title(map: &str, kind: VoteKind) -> &str {
    match (kind, map) {
        (VoteKind::Extend, EXTEND_MAP) => "Yes",
        (VoteKind::Extend, CONTINUE_MAP) => "No",
        (_, EXTEND_MAP) => "Extend by 15 minutes",
        (_, CONTINUE_MAP) => "Continue current map",
        (_, name) => map_title(name),
    }
}

fn vote_title(vote: &VoteInfo) -> &'static str {
    match vote.kind {
        VoteKind::Extend => "Extend map by 15 minutes?",
        VoteKind::Map if vote.runoff => "Map vote · final choice",
        VoteKind::Map => "Next map",
    }
}

const RECORDS_PAGE_SIZE: usize = 6;

fn records(parent: &mut ChildSpawnerCommands, online: &Online, course_name: &str) {
    let offset = online.board_page * RECORDS_PAGE_SIZE;
    heading(parent, "Verified leaderboards", course_name);
    horizontal(parent, |row| {
        button(
            row,
            "Map records",
            Action::RefreshBoard,
            !online.show_stage_records && !online.show_progression,
        );
        if catalog::find(course_name).is_some_and(|entry| entry.layout == "staged") {
            button(
                row,
                "Stage records",
                Action::StageRecords,
                online.show_stage_records,
            );
        }
        button(row, "Save eligible run", Action::Submit, false);
        for mode in [MapMode::Surf, MapMode::Bhop] {
            button(
                row,
                &format!("{} rankings", mode.label()),
                Action::Progression(mode),
                online.show_progression && online.progression_mode == mode,
            );
        }
    });
    if !online.record_status.is_empty() {
        parent.spawn(label(&online.record_status, 14., ACCENT));
    }
    if online.show_progression {
        if let Some(progression) = &online.progression {
            if let Some(own) = &progression.own {
                parent.spawn(label(
                    format!(
                        "Your {} rank · {} · {} XP",
                        progression.mode.label(),
                        own.stats.rank.label_for(progression.mode),
                        own.stats.points
                    ),
                    14.,
                    ACCENT,
                ));
            }
            if progression.total == 0 {
                parent.spawn(label("No ranked completions yet.", 16., MUTED));
            }
            for (index, player) in progression
                .entries
                .iter()
                .enumerate()
                .skip(offset)
                .take(RECORDS_PAGE_SIZE)
            {
                line(parent, |row| {
                    flexible(row, format!("{:02}    {}", index + 1, player.name), 16.);
                    row.spawn(label(
                        format!(
                            "{} · {} XP",
                            player.stats.rank.label_for(progression.mode),
                            player.stats.points
                        ),
                        14.,
                        ACCENT,
                    ));
                    button(
                        row,
                        "Profile",
                        Action::ViewProfile(player.account_id),
                        false,
                    );
                });
            }
            record_pages(parent, online.board_page, progression.entries.len());
        }
    } else if online.show_stage_records {
        if let Some(records) = online
            .stage_records
            .as_ref()
            .filter(|records| records.map.eq_ignore_ascii_case(course_name))
        {
            if records.rows.is_empty() {
                parent.spawn(label("No verified stage times yet.", 16., MUTED));
            }
            for record in records.rows.iter().skip(offset).take(RECORDS_PAGE_SIZE) {
                let personal = online
                    .stage_bests
                    .as_ref()
                    .filter(|bests| bests.map == records.map && bests.bonus == records.bonus)
                    .and_then(|bests| bests.rows.iter().find(|row| row.stage == record.stage));
                line(parent, |row| {
                    flexible(
                        row,
                        format!("Stage {} · {}", record.stage, record.row.name),
                        16.,
                    );
                    row.spawn(label(time(record.row.time_us), 16., ACCENT));
                    if let Some(personal) = personal {
                        row.spawn(label(
                            format!("PB {}", time(personal.row.time_us)),
                            14.,
                            MUTED,
                        ));
                    }
                    button(row, "Leaderboard", Action::StageBoard(record.stage), false);
                });
            }
            record_pages(parent, online.board_page, records.rows.len());
        }
    } else if let Some(board) = online
        .board
        .as_ref()
        .filter(|board| board.map.eq_ignore_ascii_case(course_name))
    {
        let mut name = board.bonus.map_or_else(
            || board.map.clone(),
            |bonus| format!("{} · Bonus {bonus}", board.map),
        );
        if let Some(stage) = board.stage {
            name.push_str(&format!(" · Stage {stage}"));
        }
        parent.spawn(label(name, 14., MUTED));
        if board.rows.is_empty() {
            parent.spawn(label("No verified times yet.", 16., MUTED));
        }
        for (index, record) in board
            .rows
            .iter()
            .enumerate()
            .skip(offset)
            .take(RECORDS_PAGE_SIZE)
        {
            line(parent, |row| {
                flexible(row, format!("{:02}    {}", index + 1, record.name), 16.);
                row.spawn(label(time(record.time_us), 16., ACCENT));
                if let Some(account) = record.account_id {
                    button(row, "Profile", Action::ViewProfile(account), false);
                }
                if let Some(span) = record.stage_span {
                    button(
                        row,
                        "Watch stage",
                        Action::WatchStage(record.id.clone(), span),
                        false,
                    );
                } else {
                    button(row, "Watch", Action::Watch(record.id.clone()), false);
                    button(row, "Ghost", Action::Ghost(record.id.clone()), false);
                }
            });
        }
        record_pages(parent, online.board_page, board.rows.len());
    }
}

fn record_pages(parent: &mut ChildSpawnerCommands, page: usize, rows: usize) {
    let pages = rows.div_ceil(RECORDS_PAGE_SIZE);
    if pages > 1 {
        horizontal(parent, |row| {
            if page > 0 {
                button(row, "Previous", Action::RecordsPage(page - 1), false);
            }
            row.spawn(label(format!("{} / {pages}", page + 1), 14., MUTED));
            if page + 1 < pages {
                button(row, "Next", Action::RecordsPage(page + 1), false);
            }
        });
    }
}

fn profile(parent: &mut ChildSpawnerCommands, online: &Online) {
    if online.web_history.visible && online.viewed_profile.is_none() {
        surfd_history(parent, online);
        return;
    }
    heading(
        parent,
        "Profile & loadout",
        "Accounts on your records service",
    );
    if online.profile_runs.is_some() {
        profile_runs(parent, online);
        return;
    }
    if let Some((profile, playtime)) = &online.viewed_profile {
        parent.spawn(label(&profile.name, 24., ACCENT));
        profile_details(parent, profile);
        if let Some(playtime) = playtime {
            parent.spawn(label(playtime_label(playtime, false), 14., MUTED));
        }
        horizontal(parent, |row| {
            button(
                row,
                "Runs",
                Action::ProfileRuns(PlayerRunSort::BestPlace, 0),
                true,
            );
            button(row, "My profile", Action::OwnProfile, false);
        });
    } else if let Some(profile) = &online.profile {
        parent.spawn(label(&profile.name, 24., ACCENT));
        profile_ranks(parent, profile);
        horizontal(parent, |row| {
            button(
                row,
                "Runs",
                Action::ProfileRuns(PlayerRunSort::BestPlace, 0),
                false,
            );
            button(row, "Surfd.net history", Action::OpenSurfdHistory, false);
        });
        horizontal(parent, |row| {
            field(row, online, Field::Motto, "Motto", 370.);
            field(row, online, Field::Title, "Title", 215.);
        });
        horizontal(parent, |row| {
            field(row, online, Field::Banner, "Banner map", 215.);
            field(
                row,
                online,
                Field::Showcase,
                "Showcase maps (comma-separated)",
                365.,
            );
            button(row, "Save profile", Action::SaveProfile, true);
        });
        parent.spawn(label(
            "Finish a map to unlock its banner and showcase. Pin up to three maps.",
            14.,
            MUTED,
        ));
        if let Some(playtime) = &online.playtime {
            horizontal(parent, |row| {
                row.spawn(label(playtime_label(playtime, true), 14., MUTED));
                button(
                    row,
                    if playtime.hidden {
                        "Show playtime"
                    } else {
                        "Hide playtime"
                    },
                    Action::SetPlaytimePrivacy(!playtime.hidden),
                    false,
                );
            });
        }
        horizontal(parent, |row| {
            button(row, "Sign out", Action::SignOut, false);
            button(row, "Delete account…", Action::ConfirmDelete, false);
            if online.confirm_delete {
                button(
                    row,
                    "Confirm account deletion",
                    Action::DeleteAccount,
                    false,
                );
            }
        });
    } else {
        field(
            parent,
            online,
            Field::Endpoint,
            "Records service address",
            410.,
        );
        horizontal(parent, |row| {
            field(row, online, Field::Name, "Username", 220.);
            field(row, online, Field::Password, "Password", 245.);
        });
        horizontal(parent, |row| {
            button(row, "Sign in", Action::SignIn, true);
            button(row, "Create account", Action::Register, false);
        });
        horizontal(parent, |row| {
            if online.provider_sign_in_pending() {
                button(row, "Cancel sign-in", Action::CancelProviderSignIn, false);
            } else {
                button(
                    row,
                    "Continue with Google",
                    Action::ProviderSignIn(OAuthProvider::Google),
                    false,
                );
                button(
                    row,
                    "Continue with X",
                    Action::ProviderSignIn(OAuthProvider::X),
                    false,
                );
            }
        });
    }
    if !online.record_status.is_empty() {
        parent.spawn(label(&online.record_status, 14., ACCENT));
    }
}

fn surfd_history(parent: &mut ChildSpawnerCommands, online: &Online) {
    let history = &online.web_history;
    heading(
        parent,
        "Surfd.net history",
        "Public web progress saved for this desktop profile on this device.",
    );
    button(parent, "Back to profile", Action::CloseSurfdHistory, false);
    if !history.busy {
        parent.spawn(label(
            "On Surfd.net, open Profile & loadout and choose Copy profile link.",
            14.,
            MUTED,
        ));
        horizontal(parent, |row| {
            field(
                row,
                online,
                Field::SurfdProfile,
                "Surfd.net profile link",
                420.,
            );
            button(row, "Paste link", Action::PasteSurfdProfile, false);
            button(row, "Load history", Action::LoadSurfdHistory, true);
            if history.snapshot.is_some() {
                button(row, "Forget history", Action::ForgetSurfdHistory, false);
            }
        });
    }
    if !history.status.is_empty() {
        parent.spawn(label(&history.status, 14., ACCENT));
    }
    let Some(snapshot) = &history.snapshot else {
        return;
    };
    if let Some(player) = &snapshot.player {
        parent.spawn(label(&player.username, 24., ACCENT));
        parent.spawn(label(
            format!(
                "{} · {} points · {} maps completed · {} map records",
                player.rank, player.points, player.completions, player.records,
            ),
            16.,
            TEXT,
        ));
        if let Some(position) = player.position {
            parent.spawn(label(
                format!("Surfd.net player ranking #{position}"),
                14.,
                MUTED,
            ));
        }
    } else {
        parent.spawn(label(
            "Surfd.net has no published progression for this profile.",
            15.,
            MUTED,
        ));
    }
    horizontal(parent, |row| {
        button(
            row,
            "Run history",
            Action::ShowSurfdScores(false),
            !history.show_scores,
        );
        button(
            row,
            "Map scores",
            Action::ShowSurfdScores(true),
            history.show_scores,
        );
    });
    let rows = if history.show_scores {
        snapshot.maps.len()
    } else {
        snapshot.runs.len()
    };
    if rows == 0 {
        let message = if history.show_scores {
            "No published map scores."
        } else {
            "No public runs found."
        };
        parent.spawn(label(message, 15., MUTED));
        return;
    }
    let pages = rows.div_ceil(SURFD_HISTORY_PAGE_SIZE);
    horizontal(parent, |row| {
        if history.page > 0 {
            button(row, "Previous", Action::SurfdHistoryPage(-1), false);
        }
        row.spawn(label(
            format!("{rows} results · Page {} of {pages}", history.page + 1),
            15.,
            TEXT,
        ));
        if history.page + 1 < pages {
            button(row, "Next", Action::SurfdHistoryPage(1), false);
        }
    });
    if history.show_scores {
        for map in snapshot
            .maps
            .iter()
            .skip(history.page * SURFD_HISTORY_PAGE_SIZE)
            .take(SURFD_HISTORY_PAGE_SIZE)
        {
            horizontal(parent, |row| {
                row.spawn(label(map_title(&map.map_id), 16., TEXT));
                row.spawn(label(surfd_time(map.time_us), 16., ACCENT));
                row.spawn(label(
                    format!("#{} · {} points", map.position, map.points),
                    15.,
                    TEXT,
                ));
                row.spawn(label(
                    format!(
                        "{} completion + {} performance",
                        map.completion_points, map.performance_points
                    ),
                    14.,
                    MUTED,
                ));
            });
        }
        return;
    }
    for run in snapshot
        .runs
        .iter()
        .skip(history.page * SURFD_HISTORY_PAGE_SIZE)
        .take(SURFD_HISTORY_PAGE_SIZE)
    {
        let score = snapshot.maps.iter().find(|map| map.run_id == run.run_id);
        let date = run.created_at.split('T').next().unwrap_or_default();
        let replay = if run.replay_available {
            "Replay available on Surfd.net"
        } else {
            "No stored replay"
        };
        horizontal(parent, |row| {
            row.spawn(label(map_title(&run.map_id), 16., TEXT));
            row.spawn(label(surfd_time(run.time_us), 16., ACCENT));
            if let Some(score) = score {
                row.spawn(label(
                    format!("#{} · {} pts", score.position, score.points),
                    14.,
                    TEXT,
                ));
            }
            row.spawn(label(format!("{date} · {replay}"), 14., MUTED));
            if !history.busy
                && !history
                    .details
                    .as_ref()
                    .is_some_and(|details| matches!(details.splits, RunSplits::Loading))
            {
                button(
                    row,
                    "Splits",
                    Action::ShowSurfdRun(run.run_id.clone()),
                    false,
                );
            }
        });
        if let Some(details) = &history.details
            && details.run_id == run.run_id
        {
            match &details.splits {
                RunSplits::Loading => {
                    parent.spawn(label("Loading splits…", 14., MUTED));
                }
                RunSplits::Error(error) => {
                    parent.spawn(label(error, 14., MUTED));
                }
                RunSplits::Loaded(splits) if splits.is_empty() => {
                    parent.spawn(label("This run has no checkpoint splits.", 14., MUTED));
                }
                RunSplits::Loaded(splits) => {
                    horizontal(parent, |row| {
                        for (index, split) in splits.iter().enumerate() {
                            row.spawn(label(
                                format!("Split {} · {}", index + 1, surfd_time(*split)),
                                14.,
                                TEXT,
                            ));
                        }
                    });
                }
            }
        }
    }
}

fn profile_runs(parent: &mut ChildSpawnerCommands, online: &Online) {
    let view = online.profile_runs.as_ref().expect("open profile runs");
    let profile = online
        .viewed_profile
        .as_ref()
        .map(|(profile, _)| profile)
        .or(online.profile.as_ref());
    if let Some(profile) = profile {
        parent.spawn(label(
            format!("{} · Personal bests", profile.name),
            24.,
            ACCENT,
        ));
    }
    horizontal(parent, |row| {
        button(row, "Profile", Action::CloseProfileRuns, false);
        for (sort, title) in [
            (PlayerRunSort::BestPlace, "Best place"),
            (PlayerRunSort::Newest, "Newest"),
        ] {
            button(row, title, Action::ProfileRuns(sort, 0), sort == view.sort);
        }
        if online.profile_replay_busy {
            button(row, "Cancel replay", Action::CancelProfileReplay, false);
        }
    });
    match &view.result {
        None => {
            parent.spawn(label("Loading runs…", 15., MUTED));
        }
        Some(Err(error)) => {
            parent.spawn(label(error, 15., MUTED));
            button(
                parent,
                "Retry",
                Action::ProfileRuns(view.sort, view.page),
                true,
            );
        }
        Some(Ok(history)) => {
            if history.total == 0 {
                parent.spawn(label("No verified runs yet.", 15., MUTED));
            }
            for run in &history.runs {
                line(parent, |row| {
                    let mut title = map_title(&run.map).to_owned();
                    if let Some(bonus) = run.bonus {
                        title.push_str(&format!(" · Bonus {bonus}"));
                    }
                    if run.archived {
                        title.push_str(" · Old board");
                    }
                    flexible(row, title, 15.);
                    if let Some(tier) = catalog::find(&run.map).and_then(|entry| entry.tier) {
                        row.spawn(label(format!("Tier {tier}"), 14., MUTED));
                    }
                    row.spawn(label(time(run.time_us), 16., ACCENT));
                    if let Some(place) = run.place {
                        row.spawn(label(format!("#{place}"), 14., TEXT));
                    }
                    if let Some(points) = run.points {
                        let mode = MapMode::from_map_name(&run.map);
                        row.spawn(label(format!("{} · {points} XP", mode.label()), 14., MUTED));
                    }
                    if let Some(date) = run
                        .created_at
                        .as_deref()
                        .and_then(|stamp| stamp.split_once('T').map(|(date, _)| date))
                    {
                        row.spawn(label(date, 14., MUTED));
                    }
                    if run.replay_available && !online.profile_replay_busy {
                        let confirmation = online
                            .profile_replay_confirm
                            .as_ref()
                            .filter(|(id, _)| id == &run.id);
                        if let Some((_, ghost)) = confirmation {
                            button(
                                row,
                                if *ghost {
                                    "Race anyway"
                                } else {
                                    "Watch anyway"
                                },
                                Action::ProfileReplay(run.id.clone(), *ghost),
                                true,
                            );
                            button(row, "Cancel", Action::CancelProfileReplay, false);
                        } else {
                            button(
                                row,
                                "Watch",
                                Action::ProfileReplay(run.id.clone(), false),
                                false,
                            );
                            button(
                                row,
                                "Race",
                                Action::ProfileReplay(run.id.clone(), true),
                                false,
                            );
                        }
                    } else if !run.replay_available {
                        row.spawn(label("Replay unavailable", 13., MUTED));
                    }
                });
            }
            let pages = history.total.div_ceil(PLAYER_RUNS_PAGE_SIZE);
            if pages > 1 {
                horizontal(parent, |row| {
                    if view.page > 0 {
                        button(
                            row,
                            "Previous",
                            Action::ProfileRuns(view.sort, view.page - 1),
                            false,
                        );
                    }
                    row.spawn(label(
                        format!("{} / {pages}", u64::from(view.page) + 1),
                        14.,
                        MUTED,
                    ));
                    if (view.page as usize).saturating_add(1) < pages {
                        button(
                            row,
                            "Next",
                            Action::ProfileRuns(view.sort, view.page + 1),
                            false,
                        );
                    }
                });
            }
        }
    }
    if !online.record_status.is_empty() {
        parent.spawn(label(&online.record_status, 14., ACCENT));
    }
}

fn profile_details(parent: &mut ChildSpawnerCommands, profile: &Profile) {
    profile_ranks(parent, profile);
    for detail in [&profile.title, &profile.motto]
        .into_iter()
        .filter(|detail| !detail.is_empty())
    {
        parent.spawn(label(detail, 16., TEXT));
    }
    if !profile.banner.is_empty() {
        parent.spawn(label(
            format!("Banner · {}", map_title(&profile.banner)),
            15.,
            ACCENT,
        ));
    }
    for map in &profile.showcase {
        parent.spawn(label(format!("Showcase · {}", map_title(map)), 14., TEXT));
    }
}

fn profile_ranks(parent: &mut ChildSpawnerCommands, profile: &Profile) {
    for (mode, stats) in [
        (MapMode::Surf, profile.progression),
        (MapMode::Bhop, profile.bhop_progression),
    ] {
        parent.spawn(label(
            format!(
                "{} · {} · {} XP · {} completed maps",
                mode.label(),
                stats.rank.label_for(mode),
                stats.points,
                stats.completions
            ),
            15.,
            ACCENT,
        ));
    }
}

fn map_title(map: &str) -> &str {
    catalog::find(map).map_or(map, |entry| entry.name.as_str())
}

fn playtime_label(playtime: &Playtime, own: bool) -> String {
    let Some(seconds) = playtime.seconds else {
        return "Playtime hidden".into();
    };
    let time = if seconds < 60 {
        "<1m".into()
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h {}m", seconds / 3600, (seconds / 60) % 60)
    };
    let visibility = if own && playtime.hidden {
        " · private"
    } else {
        ""
    };
    format!("Playtime · {time}{visibility}")
}

fn friends(parent: &mut ChildSpawnerCommands, online: &Online) {
    let Some(profile) = &online.profile else {
        heading(parent, "Friends", "Sign in to use friends.");
        return;
    };
    if let Some(page) = online
        .message_reports
        .as_ref()
        .filter(|_| online.social.can_review_message_reports)
    {
        message_reports(parent, online, page);
        return;
    }
    if online.show_message_archive {
        heading(
            parent,
            "Past conversations",
            "Messages you received before a conversation ended",
        );
        if !online.message_report_busy {
            button(
                parent,
                "Back to friends",
                Action::ShowMessageArchive(false),
                false,
            );
        }
        if !online.message_report_status.is_empty() {
            parent.spawn(label(&online.message_report_status, 14., ACCENT));
        }
        if online.social.archived_messages.is_empty() {
            parent.spawn(label("No recent messages.", 14., MUTED));
        }
        for message in &online.social.archived_messages {
            message_row(parent, online, message, profile.id);
        }
        return;
    }
    heading(
        parent,
        "Friends",
        "Requests, room invitations and direct messages",
    );
    horizontal(parent, |row| {
        field(row, online, Field::FriendName, "Username", 220.);
        button(row, "Send request", Action::RequestFriend, true);
        button(row, "Refresh", Action::Social(SocialAction::Fetch), false);
        if !online.social.archived_messages.is_empty() && !online.message_report_busy {
            button(
                row,
                "Past conversations",
                Action::ShowMessageArchive(true),
                false,
            );
        }
        if online.social.can_review_message_reports && !online.message_report_busy {
            button(
                row,
                "Review message reports",
                Action::ReviewMessageReports(None),
                false,
            );
        }
    });
    if !online.record_status.is_empty() {
        parent.spawn(label(&online.record_status, 14., ACCENT));
    }
    if !online.message_report_status.is_empty() {
        parent.spawn(label(&online.message_report_status, 14., ACCENT));
    }
    for user in online.social.incoming.iter().take(4) {
        line(parent, |row| {
            flexible(row, format!("Friend request from {}", user.name), 15.);
            button(
                row,
                "Accept",
                Action::Social(SocialAction::Accept { account: user.id }),
                true,
            );
            button(
                row,
                "Decline",
                Action::Social(SocialAction::Decline { account: user.id }),
                false,
            );
        });
    }
    for user in online.social.outgoing.iter().take(3) {
        line(parent, |row| {
            flexible(row, format!("Requested {}", user.name), 15.);
            button(
                row,
                "Cancel",
                Action::Social(SocialAction::Cancel { account: user.id }),
                false,
            );
        });
    }
    for user in online.social.friends.iter().take(6) {
        line(parent, |row| {
            let activity = user
                .presence
                .as_ref()
                .and_then(|presence| presence.activity.as_ref());
            let status = match activity {
                Some(PlayerActivity::Menu) => "In menu".into(),
                Some(PlayerActivity::Solo { map }) => format!("Solo · {}", map_title(map)),
                Some(PlayerActivity::Room { map, .. }) => format!("Online · {}", map_title(map)),
                None => "Offline".into(),
            };
            flexible(row, format!("{} · {status}", user.name), 15.);
            button(row, "Profile", Action::ViewProfile(user.id), false);
            if let Some(PlayerActivity::Room { room, .. }) = activity {
                button(row, "Join", Action::JoinAddress(room.clone()), true);
            }
            button(
                row,
                "Messages",
                Action::SelectFriend(user.id),
                online.selected_friend == Some(user.id),
            );
            if online.is_connected() {
                button(row, "Invite", Action::Invite(user.id), false);
            }
            button(
                row,
                "Remove",
                Action::Social(SocialAction::Remove { account: user.id }),
                false,
            );
            button(
                row,
                "Block",
                Action::Social(SocialAction::Block { account: user.id }),
                false,
            );
        });
    }
    for user in online.social.blocked.iter().take(3) {
        line(parent, |row| {
            flexible(row, format!("Blocked · {}", user.name), 15.);
            button(
                row,
                "Unblock",
                Action::Social(SocialAction::Unblock { account: user.id }),
                false,
            );
        });
    }
    for notice in online
        .social
        .notifications
        .iter()
        .filter(|notice| matches!(notice.kind, NotificationKind::ServerInvite))
        .take(4)
    {
        line(parent, |row| {
            flexible(
                row,
                format!("Server invitation from {}", notice.from.name),
                15.,
            );
            if let Some(address) = &notice.text {
                button(row, "Join", Action::JoinAddress(address.clone()), true);
            }
            button(
                row,
                "Dismiss",
                Action::Social(SocialAction::Dismiss {
                    notification: notice.id,
                }),
                false,
            );
        });
    }
    if let Some(user) = online
        .social
        .friends
        .iter()
        .find(|friend| Some(friend.id) == online.selected_friend)
    {
        parent.spawn(label(
            format!("Conversation with {}", user.name),
            16.,
            ACCENT,
        ));
        let recent: Vec<_> = online
            .social
            .messages
            .iter()
            .filter(|message| message.from.id == user.id || message.to.id == user.id)
            .take(6)
            .collect();
        for message in recent.into_iter().rev() {
            message_row(parent, online, message, profile.id);
        }
        horizontal(parent, |row| {
            field(row, online, Field::DirectMessage, "Direct message", 365.);
            button(row, "Send", Action::DirectMessage, true);
        });
    }
}

fn message_row(
    parent: &mut ChildSpawnerCommands,
    online: &Online,
    message: &DirectMessage,
    actor: i64,
) {
    let sender = if message.from.id == actor {
        "You"
    } else {
        &message.from.name
    };
    line(parent, |row| {
        flexible(row, format!("{sender}: {}", message.text), 14.);
        if message.to.id == actor
            && !online.message_report_busy
            && online.reporting_message != Some(message.id)
        {
            button(
                row,
                "Report",
                Action::SelectMessageReport(Some(message.id)),
                false,
            );
        }
    });
    if online.reporting_message == Some(message.id) && !online.message_report_busy {
        parent.spawn(label(
            "Choose a reason. A moderator will see this message.",
            13.,
            MUTED,
        ));
        horizontal(parent, |row| {
            for reason in MessageReportReason::ALL {
                button(
                    row,
                    reason.label(),
                    Action::SendMessageReport(reason),
                    false,
                );
            }
            button(row, "Cancel", Action::SelectMessageReport(None), false);
        });
    }
}

fn message_reports(parent: &mut ChildSpawnerCommands, online: &Online, page: &MessageReportPage) {
    heading(
        parent,
        "Message reports",
        "Reports from the past seven days",
    );
    if !online.message_report_status.is_empty() {
        parent.spawn(label(&online.message_report_status, 14., ACCENT));
    }
    if !online.message_report_busy {
        horizontal(parent, |row| {
            button(row, "Back to friends", Action::CloseMessageReports, false);
            button(row, "Newest", Action::ReviewMessageReports(None), false);
            if let Some(before) = page.next_before {
                button(
                    row,
                    "Older",
                    Action::ReviewMessageReports(Some(before)),
                    false,
                );
            }
        });
    }
    if page.reports.is_empty() {
        parent.spawn(label("No reports on this page.", 14., MUTED));
    }
    for report in &page.reports {
        line(parent, |row| {
            flexible(
                row,
                format!(
                    "{} · {} reported {}",
                    report.reason.label(),
                    report.reporter.name,
                    report.sender.name
                ),
                14.,
            );
            button(
                row,
                "Sender profile",
                Action::ViewProfile(report.sender.id),
                false,
            );
        });
        line(parent, |row| flexible(row, &report.text, 14.));
    }
}

fn moderation(parent: &mut ChildSpawnerCommands, online: &Online) {
    let moderator = online
        .room_identity()
        .is_some_and(|identity| identity.moderator);
    if let Some(page) = online.room_reports.as_ref().filter(|_| moderator) {
        heading(
            parent,
            "Room reports",
            "Reports and chat excerpts from the past seven days",
        );
        horizontal(parent, |row| {
            button(row, "Back to players", Action::CloseRoomReports, false);
            button(row, "Refresh", Action::ReviewRoomReports(0), false);
            if page.offset > 0 {
                button(
                    row,
                    "Previous",
                    Action::ReviewRoomReports(page.offset.saturating_sub(ROOM_REPORT_PAGE_SIZE)),
                    false,
                );
            }
            if page.offset + ROOM_REPORT_PAGE_SIZE < page.total {
                button(
                    row,
                    "Next",
                    Action::ReviewRoomReports(page.offset + ROOM_REPORT_PAGE_SIZE),
                    false,
                );
            }
        });
        if page.reports.is_empty() {
            parent.spawn(label("No reports to review.", 15., MUTED));
        }
        for report in &page.reports {
            parent.spawn(label(
                format!("{} reported {}", report.reporter.name, report.target.name),
                16.,
                ACCENT,
            ));
            parent.spawn(label(&report.reason, 15., TEXT));
            for line in &report.excerpts {
                parent.spawn(label(&line.text, 13., MUTED));
            }
        }
        return;
    }
    heading(
        parent,
        "Moderation",
        if moderator {
            "Manage this room and its players."
        } else {
            "Send a player report to the room host."
        },
    );
    if moderator {
        horizontal(parent, |row| {
            field(row, online, Field::Nomination, "Map in room rotation", 250.);
            button(row, "Change map", Action::RoomChangeMap, true);
            button(row, "Review reports", Action::ReviewRoomReports(0), false);
        });
    }
    if online.is_connected() {
        field(parent, online, Field::ReportReason, "What happened?", 420.);
        parent.spawn(label(&online.status, 14., MUTED));
    }
    for player in online
        .players
        .iter()
        .filter(|player| Some(player.id) != online.id)
        .take(10)
    {
        line(parent, |row| {
            flexible(row, &player.name, 16.);
            button(row, "Report player", Action::Report(player.id), false);
            if moderator && player.identity.is_some_and(|identity| !identity.moderator) {
                button(
                    row,
                    "Kick",
                    Action::RoomModerate(player.id, RoomModeration::Kick),
                    false,
                );
                if player
                    .identity
                    .is_some_and(|identity| identity.account_id.is_some())
                {
                    button(
                        row,
                        "Mute 10m",
                        Action::RoomModerate(player.id, RoomModeration::Mute),
                        false,
                    );
                    button(
                        row,
                        "Unmute",
                        Action::RoomModerate(player.id, RoomModeration::Unmute),
                        false,
                    );
                    button(
                        row,
                        "Ban",
                        Action::RoomModerate(player.id, RoomModeration::Ban),
                        false,
                    );
                }
            }
        });
    }
}

fn time(micros: u64) -> String {
    let millis = micros / 1000;
    format!(
        "{:02}:{:02}.{:03}",
        millis / 60_000,
        millis / 1000 % 60,
        millis % 1000
    )
}

pub(crate) fn surfd_time(micros: u64) -> String {
    // Surfd's public timer rounds to the nearest millisecond.
    let millis = micros / 1000 + u64::from(micros % 1000 >= 500);
    format!(
        "{:02}:{:02}.{:03}",
        millis / 60_000,
        millis / 1000 % 60,
        millis % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::surfd_time;

    #[test]
    fn public_times_match_surfd_rounding_at_milliseconds_and_minute_rollover() {
        assert_eq!(surfd_time(39_732_934), "00:39.733");
        assert_eq!(surfd_time(45_306_499), "00:45.306");
        assert_eq!(surfd_time(45_306_500), "00:45.307");
        assert_eq!(surfd_time(59_999_500), "01:00.000");
    }
}

fn heading(parent: &mut ChildSpawnerCommands, title: &str, detail: &str) {
    parent.spawn(label(title, 32., TEXT));
    parent.spawn(label(detail, 15., MUTED));
}

fn label(text: impl Into<String>, size: f32, color: Color) -> (Text, TextFont, TextColor) {
    (
        Text::new(text),
        TextFont {
            font_size: size.into(),
            ..default()
        },
        TextColor(color),
    )
}

fn flexible(parent: &mut ChildSpawnerCommands, value: impl Into<String>, size: f32) {
    parent.spawn((
        Node {
            flex_grow: 1.0,
            min_width: px(0),
            ..default()
        },
        label(value, size, TEXT),
    ));
}

fn horizontal(parent: &mut ChildSpawnerCommands, children: impl FnOnce(&mut ChildSpawnerCommands)) {
    parent
        .spawn(Node {
            flex_wrap: FlexWrap::Wrap,
            column_gap: px(8),
            row_gap: px(8),
            align_items: AlignItems::Center,
            ..default()
        })
        .with_children(children);
}

fn line(parent: &mut ChildSpawnerCommands, children: impl FnOnce(&mut ChildSpawnerCommands)) {
    parent
        .spawn((
            Node {
                width: percent(100),
                max_width: px(900),
                column_gap: px(8),
                padding: UiRect::all(px(6)),
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(PANEL),
        ))
        .with_children(children);
}

fn button(parent: &mut ChildSpawnerCommands, title: &str, action: Action, primary: bool) {
    let color = if primary { ACCENT } else { TEXT };
    parent
        .spawn((
            OnlineButton(action),
            Node {
                padding: UiRect::axes(px(10), px(6)),
                border: UiRect::all(px(1)),
                ..default()
            },
            BackgroundColor(BUTTON),
            BorderColor::all(if primary { ACCENT } else { EDGE }),
        ))
        .observe(click)
        .with_child((label(title, 14., color), Pickable::IGNORE));
}

fn field(parent: &mut ChildSpawnerCommands, online: &Online, kind: Field, title: &str, width: f32) {
    let focused = online.field == Some(kind);
    let value = online.field_value(kind);
    let value = if kind == Field::Password {
        "•".repeat(value.chars().count())
    } else {
        value.to_owned()
    };
    let content = if focused {
        format!("{value}▏")
    } else if value.is_empty() {
        "Click to type".into()
    } else {
        value
    };
    parent
        .spawn((
            OnlineButton(Action::Edit(kind)),
            Node {
                width: px(width),
                padding: UiRect::all(px(7)),
                border: UiRect::all(px(1)),
                flex_direction: FlexDirection::Column,
                row_gap: px(2),
                ..default()
            },
            BackgroundColor(PANEL),
            BorderColor::all(if focused { ACCENT } else { EDGE }),
        ))
        .observe(click)
        .with_children(|input| {
            input.spawn((label(title, 11., MUTED), Pickable::IGNORE));
            input.spawn((label(content, 14., TEXT), Pickable::IGNORE));
        });
}

fn click(event: On<PointerClick>, buttons: Query<&OnlineButton>, mut online: ResMut<Online>) {
    if event.button == PointerButton::Primary
        && let Ok(button) = buttons.get(event.entity)
    {
        online.queue(button.0.clone());
    }
}
