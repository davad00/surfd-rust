use bevy::{math::Rot2, ui::UiTransform};

use super::*;
use crate::cosmetics::KnifePalette;

enum Preview {
    Avatar([u8; 3]),
    Knife(KnifePalette),
}

struct Item {
    name: &'static str,
    preview: Preview,
    equipped: bool,
    equip: Action,
}

pub(super) fn render(parent: &mut ChildSpawnerCommands, settings: &Settings) {
    section(
        parent,
        "Player skins",
        AvatarSkin::ALL.map(|skin| Item {
            name: skin.label(),
            preview: Preview::Avatar(skin.swatch()),
            equipped: settings.avatar_skin == skin,
            equip: Action::EquipAvatar(skin),
        }),
    );
    section(
        parent,
        "Knife finishes",
        KnifeFinish::ALL.map(|finish| Item {
            name: finish.label(),
            preview: Preview::Knife(finish.palette()),
            equipped: settings.knife_finish == finish,
            equip: Action::EquipKnife(finish),
        }),
    );
    let camera = settings.primary_binding(ControlAction::ToggleCamera);
    let hint = if camera == "—" {
        "Skins appear in third person and the knife appears in first person. Choices save on this device.".into()
    } else {
        format!(
            "{camera} switches between your player skin and first-person knife. Choices save on this device."
        )
    };
    parent.spawn(text(hint, 12., MUTED));
}

fn section(parent: &mut ChildSpawnerCommands, title: &str, items: [Item; 3]) {
    parent.spawn(text(title, 19., INK));
    parent
        .spawn(Node {
            width: percent(100),
            column_gap: px(8),
            row_gap: px(8),
            flex_wrap: FlexWrap::Wrap,
            ..default()
        })
        .with_children(|row| {
            for item in items {
                card(row, item);
            }
        });
}

fn card(parent: &mut ChildSpawnerCommands, item: Item) {
    parent
        .spawn((
            Node {
                width: percent(32.),
                min_width: px(152),
                flex_grow: 1.,
                flex_direction: FlexDirection::Column,
                border: UiRect::all(px(1)),
                ..default()
            },
            BackgroundColor(PANEL),
            BorderColor::all(if item.equipped { MINT } else { EDGE }),
        ))
        .with_children(|card| {
            card.spawn((
                Node {
                    height: px(84),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    overflow: Overflow::clip(),
                    border: UiRect::bottom(px(1)),
                    ..default()
                },
                BackgroundGradient(vec![Gradient::Linear(LinearGradient::to_bottom(vec![
                    ColorStop::auto(TILE),
                    ColorStop::auto(INSET),
                ]))]),
                BorderColor::all(EDGE),
            ))
            .with_children(|preview| match item.preview {
                Preview::Avatar(rgb) => avatar_preview(preview, rgb),
                Preview::Knife(palette) => knife_preview(preview, palette),
            });
            card.spawn(Node {
                padding: UiRect::axes(px(9), px(6)),
                column_gap: px(5),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                ..default()
            })
            .with_children(|details| {
                details.spawn(text(item.name, 15., INK));
                if item.equipped {
                    details.spawn((
                        Node {
                            padding: UiRect::axes(px(4), px(5)),
                            ..default()
                        },
                        text("Equipped", 12., MINT),
                    ));
                } else {
                    button(details, "Equip", item.equip, false);
                }
            });
        });
}

fn avatar_preview(parent: &mut ChildSpawnerCommands, rgb: [u8; 3]) {
    parent
        .spawn((
            Node {
                width: px(72),
                height: px(76),
                ..default()
            },
            Pickable::IGNORE,
        ))
        .with_children(|shape| {
            let dark = shade(rgb, 58);
            let jacket = shade(rgb, 94);
            let highlight = shade(rgb, 117);
            piece(shape, 22., 47., 13., 22., dark);
            piece(shape, 38., 47., 13., 22., dark);
            piece(shape, 20., 67., 16., 6., INSET);
            piece(shape, 38., 67., 16., 6., INSET);
            piece(shape, 20., 23., 33., 27., jacket);
            piece(shape, 23., 23., 26., 4., highlight);
            piece(shape, 12., 25., 10., 22., dark);
            piece(shape, 51., 25., 10., 22., dark);
            piece(shape, 12., 44., 9., 5., INSET);
            piece(shape, 52., 44., 9., 5., INSET);
            piece(shape, 29., 7., 15., 16., Color::srgb_u8(194, 153, 129));
            piece(shape, 28., 5., 16., 6., Color::srgb_u8(44, 40, 39));
            piece(shape, 35., 26., 2., 20., dark);
            piece(shape, 41., 29., 6., 4., highlight);
        });
}

fn knife_preview(parent: &mut ChildSpawnerCommands, palette: KnifePalette) {
    parent
        .spawn((
            Node {
                width: px(116),
                height: px(38),
                ..default()
            },
            UiTransform::from_rotation(Rot2::radians(-0.38)),
            Pickable::IGNORE,
        ))
        .with_children(|shape| {
            piece(shape, 10., 13., 34., 13., shade(palette.handle, 88));
            for x in [14., 22., 30.] {
                piece(shape, x, 15., 3., 8., shade(palette.grip, 133));
            }
            piece(shape, 41., 9., 5., 22., shade(palette.handle, 132));
            piece(shape, 46., 12., 43., 14., shade(palette.blade, 102));
            piece(shape, 89., 12., 9., 11., shade(palette.blade, 102));
            piece(shape, 98., 12., 6., 7., shade(palette.blade, 102));
            piece(shape, 104., 12., 4., 4., shade(palette.bevel, 110));
            piece(shape, 46., 23., 44., 3., shade(palette.bevel, 108));
        });
}

fn piece(parent: &mut ChildSpawnerCommands, x: f32, y: f32, w: f32, h: f32, color: Color) {
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

fn shade(rgb: [u8; 3], percent: u16) -> Color {
    let [r, g, b] = rgb.map(|channel| ((u16::from(channel) * percent / 100).min(255)) as u8);
    Color::srgb_u8(r, g, b)
}
