use bevy::{color::ColorToPacked, math::Rot2, mesh::VertexAttributeValues};

use super::*;
use crate::view::knife_view::world_knife_mesh;

const KNIFE_PREVIEW_SIZE: UVec2 = UVec2::new(136, 72);

pub(crate) struct KnifePreviews([Handle<Image>; KnifeFinish::ALL.len()]);

impl KnifePreviews {
    pub(super) fn new(images: &mut Assets<Image>) -> Self {
        Self(KnifeFinish::ALL.map(|finish| images.add(knife_thumbnail(finish))))
    }
}

enum Preview {
    Avatar([u8; 3]),
    Knife(Handle<Image>),
}

struct Item {
    name: &'static str,
    preview: Preview,
    equipped: bool,
    equip: Action,
}

pub(super) fn render(
    parent: &mut ChildSpawnerCommands,
    settings: &Settings,
    online: &Online,
    knives: &KnifePreviews,
) {
    let loadout = online.equipped_loadout(settings);
    section(
        parent,
        "Player skins",
        AvatarSkin::ALL.map(|skin| Item {
            name: skin.label(),
            preview: Preview::Avatar(skin.swatch()),
            equipped: loadout.avatar_skin == skin,
            equip: Action::EquipAvatar(skin),
        }),
    );
    section(
        parent,
        "Knife finishes",
        std::array::from_fn(|index| {
            let finish = KnifeFinish::ALL[index];
            Item {
                name: finish.label(),
                preview: Preview::Knife(knives.0[index].clone()),
                equipped: loadout.knife_finish == finish,
                equip: Action::EquipKnife(finish),
            }
        }),
    );
    let camera = settings.primary_binding(ControlAction::ToggleCamera);
    let hint = if camera == "—" {
        "Skins appear in third person and the knife appears in first person.".into()
    } else {
        format!("{camera} switches between your player skin and first-person knife.")
    };
    parent.spawn(text(hint, 12., MUTED));
    parent.spawn(text(online.cosmetic_status(), 12., MUTED));
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
                Preview::Knife(image) => knife_preview(preview, image),
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

fn knife_preview(parent: &mut ChildSpawnerCommands, image: Handle<Image>) {
    parent.spawn((
        ImageNode::new(image),
        Node {
            width: px(KNIFE_PREVIEW_SIZE.x as f32),
            height: px(KNIFE_PREVIEW_SIZE.y as f32),
            ..default()
        },
        Pickable::IGNORE,
    ));
}

fn knife_thumbnail(finish: KnifeFinish) -> Image {
    let mesh = world_knife_mesh(finish);
    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        unreachable!("knife mesh has position attributes")
    };
    let Some(VertexAttributeValues::Float32x4(colors)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
    else {
        unreachable!("knife mesh has linear vertex colors")
    };

    let angle = Quat::from_euler(EulerRot::YXZ, -0.21, 0.10, 0.0);
    let tilt = Rot2::radians(-0.32);
    let mut points: Vec<_> = positions
        .iter()
        .map(|&position| {
            let view = angle * Vec3::from_array(position);
            (tilt * Vec2::new(view.y, -view.x)).extend(view.z)
        })
        .collect();
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    for point in &points {
        min = min.min(point.truncate());
        max = max.max(point.truncate());
    }
    let size = KNIFE_PREVIEW_SIZE * 2;
    let center = (min + max) * 0.5;
    let scale = ((size.as_vec2() - Vec2::splat(12.0)) / (max - min)).min_element();
    for point in &mut points {
        *point = ((point.truncate() - center) * scale + size.as_vec2() * 0.5).extend(point.z);
    }

    let pixel_count = (size.x * size.y) as usize;
    let mut depth = vec![f32::NEG_INFINITY; pixel_count];
    let mut pixels = vec![0; pixel_count * 4];
    // The shared knife mesh is a triangle list with lighting baked into its linear colors.
    for (triangle, color) in points.chunks_exact(3).zip(colors.chunks_exact(3)) {
        let [a, b, c] = [triangle[0], triangle[1], triangle[2]];
        let ab = (b - a).truncate();
        let ac = (c - a).truncate();
        let area = ab.perp_dot(ac);
        if area.abs() < 1e-5 {
            continue;
        }
        let low = a.min(b).min(c).truncate().floor().as_uvec2();
        let high = a.max(b).max(c).truncate().ceil().as_uvec2().min(size);
        let [color_a, color_b, color_c] = [color[0], color[1], color[2]].map(Vec4::from_array);
        for y in low.y..high.y {
            for x in low.x..high.x {
                let offset = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) - a.truncate();
                let beta = offset.perp_dot(ac) / area;
                let gamma = ab.perp_dot(offset) / area;
                let alpha = 1.0 - beta - gamma;
                if alpha.min(beta).min(gamma) < 0.0 {
                    continue;
                }
                let z = alpha * a.z + beta * b.z + gamma * c.z;
                let index = (y * size.x + x) as usize;
                if z > depth[index] {
                    depth[index] = z;
                    let color = alpha * color_a + beta * color_b + gamma * color_c;
                    pixels[index * 4..index * 4 + 4].copy_from_slice(
                        &Color::linear_rgba(color.x, color.y, color.z, color.w)
                            .to_srgba()
                            .to_u8_array(),
                    );
                }
            }
        }
    }
    Image::new(
        Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
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
