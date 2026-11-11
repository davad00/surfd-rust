use super::*;

const TABS: [(SettingsTab, &str, &str); 8] = [
    (
        SettingsTab::Controls,
        "Key bindings",
        "controls keys keyboard mouse scroll jump crouch restart practice diagnostics",
    ),
    (
        SettingsTab::Turns,
        "Turn binds",
        "turn yaw speed presets left right alternate cl_yawspeed",
    ),
    (
        SettingsTab::Mouse,
        "Mouse & view",
        "mouse sensitivity invert camera smooth field view fov source",
    ),
    (
        SettingsTab::Gameplay,
        "Gameplay",
        "movement auto bunnyhop unrestricted start practice position save load diagnostics replay ghost",
    ),
    (
        SettingsTab::Video,
        "Video",
        "display fullscreen window vsync vertical sync menu flythrough motion",
    ),
    (
        SettingsTab::Audio,
        "Audio",
        "audio sound map environmental volume ambient wind",
    ),
    (
        SettingsTab::Hud,
        "HUD",
        "hud scale layout key display accessibility colour blind strafe sync speed checkpoint split comparisons",
    ),
    (
        SettingsTab::Crosshair,
        "Crosshair",
        "crosshair style share code colour opacity size circle dot t shape gap outline",
    ),
];

pub(super) fn settings(parent: &mut ChildSpawnerCommands, menu: &MenuState, game: &Game) {
    let search = menu.settings_search.trim().to_ascii_lowercase();
    let visible = |name: &str, keywords: &str| {
        search
            .split_whitespace()
            .all(|part| name.to_ascii_lowercase().contains(part) || keywords.contains(part))
    };
    let active = TABS
        .iter()
        .filter(|(_, name, keywords)| visible(name, keywords))
        .find(|(tab, _, _)| *tab == menu.settings_tab)
        .or_else(|| {
            TABS.iter()
                .find(|(_, name, keywords)| visible(name, keywords))
        })
        .map(|(tab, _, _)| *tab);

    parent
        .spawn(Node {
            column_gap: px(12),
            align_items: AlignItems::Start,
            ..default()
        })
        .with_children(|layout| {
            layout
                .spawn((
                    Node {
                        width: px(170),
                        flex_shrink: 0.,
                        flex_direction: FlexDirection::Column,
                        row_gap: px(4),
                        padding: UiRect::all(px(7)),
                        ..default()
                    },
                    BackgroundColor(INSET),
                ))
                .with_children(|sidebar| {
                    field(
                        sidebar,
                        menu,
                        SettingsField::Search,
                        "Search settings",
                        &menu.settings_search,
                    );
                    sidebar.spawn(text("Controls", 12., MUTED));
                    for (tab, name, keywords) in TABS {
                        if visible(name, keywords) {
                            button(sidebar, name, Action::SettingsTab(tab), active == Some(tab));
                        }
                    }
                    button(sidebar, "Reset controls", Action::ResetBindings, false);
                    button(sidebar, "Reset settings", Action::ResetSettings, false);
                });
            layout
                .spawn((
                    MenuScroll,
                    ScrollPosition(Vec2::new(0.0, menu.settings_scroll_y)),
                    Node {
                        min_width: px(0),
                        flex_grow: 1.0,
                        max_height: vh(72),
                        flex_direction: FlexDirection::Column,
                        overflow: Overflow::scroll_y(),
                        row_gap: px(5),
                        padding: UiRect::right(px(7)),
                        ..default()
                    },
                ))
                .with_children(|pane| match active {
                    Some(SettingsTab::Controls) => controls(pane, menu),
                    Some(SettingsTab::Turns) => turns(pane, menu),
                    Some(SettingsTab::Mouse) => mouse(pane, menu),
                    Some(SettingsTab::Gameplay) => gameplay(pane, menu, game),
                    Some(SettingsTab::Video) => video(pane, menu),
                    Some(SettingsTab::Audio) => audio(pane, menu),
                    Some(SettingsTab::Hud) => hud(pane, menu),
                    Some(SettingsTab::Crosshair) => crosshair_options(pane, menu),
                    None => {
                        pane.spawn(text("No settings match that search.", 16., MUTED));
                    }
                });
        });
}

fn section(parent: &mut ChildSpawnerCommands, heading: &str, detail: &str) {
    parent.spawn(text(heading, 19., INK));
    parent.spawn(text(detail, 12., MUTED));
}

fn controls(parent: &mut ChildSpawnerCommands, menu: &MenuState) {
    section(
        parent,
        "Key bindings",
        "Select a binding, then press a key, mouse button, or scroll. Esc cancels and Backspace clears.",
    );
    for action in ControlAction::MOVEMENT {
        binding_row(parent, menu, action);
    }
}

fn turns(parent: &mut ChildSpawnerCommands, menu: &MenuState) {
    section(
        parent,
        "Turn binds",
        "Hold a bound key to turn at a fixed rate, like +left or +right in CS:S.",
    );
    toggle(
        parent,
        "Enable turn binds",
        menu.settings.turns_enabled,
        Toggle::Turns,
    );
    for action in ControlAction::TURNS {
        binding_row(parent, menu, action);
    }
    parent.spawn(text("Yaw speeds · degrees per second", 15., INK));
    parent
        .spawn(Node {
            flex_wrap: FlexWrap::Wrap,
            column_gap: px(6),
            row_gap: px(6),
            ..default()
        })
        .with_children(|presets| {
            for (index, rate) in menu.settings.yaw_speeds.iter().enumerate() {
                button(
                    presets,
                    &format!("{rate}°/s"),
                    Action::SelectYaw(index),
                    menu.settings.yaw_index == index,
                );
                if menu.settings.yaw_speeds.len() > 1 {
                    button(presets, "×", Action::RemoveYaw(index), false);
                }
            }
        });
    setting_row(parent, "Add a yaw speed", |row| {
        field(row, menu, SettingsField::AddYaw, "Add speed", "")
    });
    setting_row(parent, "Alternate yaw speed while held", |row| {
        field(
            row,
            menu,
            SettingsField::AlternateYaw,
            "Set speed",
            &format!("{}°/s", menu.settings.alternate_yaw),
        );
    });
}

fn binding_row(parent: &mut ChildSpawnerCommands, menu: &MenuState, action: ControlAction) {
    setting_row(parent, action.label(), |row| {
        let bindings = menu.settings.bindings(action);
        for (index, binding) in bindings.iter().enumerate() {
            let active = menu.capture == Some((action, Some(index)));
            let label = if active {
                "Press input…".into()
            } else {
                binding.label()
            };
            button(row, &label, Action::Capture(action, Some(index)), active);
        }
        if bindings.len() < MAX_BINDINGS {
            let active = menu.capture == Some((action, None));
            button(
                row,
                if active { "Press input…" } else { "+" },
                Action::Capture(action, None),
                active,
            );
        }
    });
}

fn mouse(parent: &mut ChildSpawnerCommands, menu: &MenuState) {
    section(
        parent,
        "Mouse & view",
        "Sensitivity uses the CS:S scale. Field of view is the horizontal angle at 4:3.",
    );
    setting_row(parent, "Sensitivity", |row| {
        button(row, "−", Action::Sensitivity(-0.1), false);
        field(
            row,
            menu,
            SettingsField::Sensitivity,
            "Set sensitivity",
            &format!("{:.2}", menu.settings.sensitivity),
        );
        button(row, "+", Action::Sensitivity(0.1), false);
    });
    setting_row(parent, "Field of view", |row| {
        button(row, "−", Action::Fov(-1.0), false);
        field(
            row,
            menu,
            SettingsField::Fov,
            "Set field of view",
            &format!("{:.0}°", menu.settings.fov),
        );
        button(row, "+", Action::Fov(1.0), false);
    });
    toggle(
        parent,
        "Invert mouse Y",
        menu.settings.invert_mouse,
        Toggle::InvertMouse,
    );
    toggle(
        parent,
        "Smooth camera between ticks",
        menu.settings.smooth_camera,
        Toggle::SmoothCamera,
    );
}

fn gameplay(parent: &mut ChildSpawnerCommands, menu: &MenuState, game: &Game) {
    section(
        parent,
        "Movement rules",
        "Changing a movement rule restarts the run.",
    );
    toggle(
        parent,
        "Auto bunnyhop",
        menu.settings.auto_bhop,
        Toggle::AutoBhop,
    );
    toggle(
        parent,
        "Unrestricted start speed · unranked",
        menu.settings.unrestricted_start,
        Toggle::UnrestrictedStart,
    );
    section(
        parent,
        "Practice",
        "Save any position on the course. Loading it begins a practice run.",
    );
    setting_row(parent, "Saved position", |row| {
        button(row, "Save", Action::SavePosition, false);
        if game.session.has_saved_position() {
            button(row, "Load", Action::RestorePosition, false);
        }
    });
    toggle(
        parent,
        "Movement diagnostics",
        menu.settings.diagnostics,
        Toggle::Diagnostics,
    );
    if game.has_replay() {
        setting_row(parent, "Command replay", |row| {
            button(
                row,
                "Watch",
                Action::Request(MenuRequest::WatchReplay),
                false,
            );
            button(
                row,
                "Export",
                Action::Request(MenuRequest::ExportReplay),
                false,
            );
        });
    }
    let original = game.course.catalog_revision.is_some()
        && game
            .selected_bonus()
            .is_none_or(|number| game.course.bonus(number).is_some());
    if game.has_replay() || original {
        setting_row(parent, "Record ghost", |row| {
            button(
                row,
                if game.ghost_enabled() { "On" } else { "Off" },
                Action::Request(MenuRequest::ToggleGhost),
                game.ghost_enabled(),
            );
            if original {
                button(
                    row,
                    "Watch original record",
                    Action::Request(MenuRequest::WatchReferenceReplay),
                    false,
                );
            }
        });
    }
}

fn video(parent: &mut ChildSpawnerCommands, menu: &MenuState) {
    section(
        parent,
        "Video",
        "Choose the display mode and vertical sync behaviour.",
    );
    toggle(
        parent,
        "Fullscreen",
        menu.settings.fullscreen,
        Toggle::Fullscreen,
    );
    toggle(parent, "Vertical sync", menu.settings.vsync, Toggle::Vsync);
    toggle(
        parent,
        "Menu flythrough motion",
        menu.settings.menu_motion,
        Toggle::MenuMotion,
    );
}

fn audio(parent: &mut ChildSpawnerCommands, menu: &MenuState) {
    section(
        parent,
        "Map audio",
        "Adjust the volume of wind and environmental ambience.",
    );
    toggle(
        parent,
        "Environmental audio",
        menu.settings.environmental_audio,
        Toggle::EnvironmentalAudio,
    );
    setting_row(parent, "Environmental volume", |row| {
        button(row, "−", Action::AdjustVolume(-0.05), false);
        field(
            row,
            menu,
            SettingsField::Volume,
            "Set volume",
            &format!("{:.0}%", menu.settings.environmental_volume * 100.0),
        );
        button(row, "+", Action::AdjustVolume(0.05), false);
    });
}

fn hud(parent: &mut ChildSpawnerCommands, menu: &MenuState) {
    section(
        parent,
        "HUD",
        "Adjust the placement and readability of run telemetry.",
    );
    setting_row(parent, "HUD scale", |row| {
        button(row, "−", Action::AdjustHud(-0.05), false);
        row.spawn(text(
            format!("{:.0}%", menu.settings.hud_scale * 100.0),
            13.,
            INK,
        ));
        button(row, "+", Action::AdjustHud(0.05), false);
    });
    for (name, enabled, item) in [
        ("Key display", menu.settings.show_keys, Toggle::Keys),
        (
            "Speed under the crosshair",
            menu.settings.center_speed,
            Toggle::CenterSpeed,
        ),
        ("Strafe sync", menu.settings.show_sync, Toggle::StrafeSync),
        (
            "Checkpoint time comparisons",
            menu.settings.compare_splits,
            Toggle::CompareSplits,
        ),
        (
            "Checkpoint speed comparisons",
            menu.settings.compare_speed,
            Toggle::CompareSpeed,
        ),
        (
            "Colour-blind mode",
            menu.settings.colorblind,
            Toggle::Colorblind,
        ),
    ] {
        toggle(parent, name, enabled, item);
    }
    button(
        parent,
        "Customise crosshair",
        Action::SettingsTab(SettingsTab::Crosshair),
        false,
    );
}

fn crosshair_options(parent: &mut ChildSpawnerCommands, menu: &MenuState) {
    section(
        parent,
        "Crosshair",
        "Adjust your crosshair or import a code shared by another player.",
    );
    toggle(
        parent,
        "Show crosshair",
        menu.settings.show_crosshair,
        Toggle::Crosshair,
    );
    parent
        .spawn((
            Node {
                width: percent(100),
                height: px(96),
                flex_shrink: 0.,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(INSET),
        ))
        .with_children(|preview| {
            crosshair(preview, &menu.settings, 3.0);
        });
    parent
        .spawn(Node {
            flex_wrap: FlexWrap::Wrap,
            row_gap: px(4),
            column_gap: px(4),
            ..default()
        })
        .with_children(|presets| {
            for (index, name) in [
                "Surfd",
                "CS:S classic",
                "Dot",
                "Cross + dot",
                "Small cross",
                "T",
                "Circle",
                "Circle + dot",
            ]
            .iter()
            .enumerate()
            {
                button(
                    presets,
                    name,
                    Action::CrosshairPreset(index),
                    menu.settings.crosshair == CrosshairStyle::preset(index),
                );
            }
        });
    for part in CrosshairPart::ALL {
        setting_row(parent, part.label(), |row| {
            button(row, "−", Action::CrosshairAdjust(part, -1), false);
            row.spawn(text(
                menu.settings.crosshair.value(part).to_string(),
                13.,
                INK,
            ));
            button(row, "+", Action::CrosshairAdjust(part, 1), false);
        });
    }
    toggle(
        parent,
        "T style",
        menu.settings.crosshair.t_shape,
        Toggle::CrosshairT,
    );
    setting_row(parent, "Colour", |row| {
        row.spawn((
            Node {
                width: px(24),
                height: px(20),
                ..default()
            },
            BackgroundColor(menu.settings.crosshair.color(menu.settings.colorblind)),
        ));
        field(
            row,
            menu,
            SettingsField::CrosshairColor,
            "Hex colour",
            &crosshair_rgb(menu.settings.crosshair),
        );
    });
    setting_row(parent, "Crosshair code", |row| {
        row.spawn(text(menu.settings.crosshair.code(), 11., MINT));
        button(row, "Copy", Action::CopyCrosshair, false);
    });
    setting_row(parent, "Import crosshair", |row| {
        field(row, menu, SettingsField::CrosshairCode, "Enter a code", "");
        button(row, "Import clipboard", Action::PasteCrosshair, false);
    });
}

fn toggle(parent: &mut ChildSpawnerCommands, title: &str, enabled: bool, value: Toggle) {
    setting_row(parent, title, |row| {
        button(
            row,
            if enabled { "On" } else { "Off" },
            Action::Toggle(value),
            enabled,
        );
    });
}

fn field(
    parent: &mut ChildSpawnerCommands,
    menu: &MenuState,
    field: SettingsField,
    hint: &str,
    value: &str,
) {
    if let Some((_, current)) = menu.edit.as_ref().filter(|(editing, _)| *editing == field) {
        let label = if current.is_empty() {
            format!("{hint} ▏")
        } else {
            format!("{current}▏")
        };
        button(parent, &label, Action::CommitEdit, true);
    } else {
        button(
            parent,
            if value.is_empty() { hint } else { value },
            Action::Edit(field),
            false,
        );
    }
}
