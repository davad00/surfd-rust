use std::sync::OnceLock;

use bevy::asset::RenderAssetUsages;
use bevy::camera::{RenderTarget, visibility::RenderLayers};
use bevy::color::ColorToPacked;
use bevy::image::ImageSampler;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use bevy::render::{
    render_resource::{
        Extent3d, TextureDataOrder, TextureDimension, TextureFormat, TextureViewDescriptor,
        TextureViewDimension,
    },
    view::{DebandDither, NoIndirectDrawing, Tonemapping},
};

use crate::cosmetics::{AvatarSkin, KnifeFinish, Loadout};
use crate::knife_asset::{self, KnifeAssets};
use crate::menu::{MenuState, Page};
use crate::online::Online;

use super::{Game, ViewCamera};

const KNIFE_LAYER: usize = 2;
const REST_POSITION: Vec3 = Vec3::new(0.31, -0.27, -0.73);
const KNIFE_PACK: &[u8] = include_bytes!("../assets/viewmodel/knife.pak");
const ARMS_PACK: &[u8] = include_bytes!("../assets/viewmodel/arms.pak");

#[derive(Component)]
pub(super) struct KnifeCamera;

#[derive(Component)]
pub(super) struct HeldKnife;

#[derive(Component)]
pub(super) struct KnifeBlade(KnifeFinish);

#[derive(Component)]
pub(super) struct KnifeArm;

#[derive(Resource)]
pub(super) struct ArmMaterials([Handle<StandardMaterial>; AvatarSkin::ALL.len()]);

#[derive(Default)]
pub(super) struct ViewmodelState {
    sway: Vec2,
    loadout: Option<Loadout>,
}

pub(super) fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    target: Single<&RenderTarget, With<ViewCamera>>,
) {
    let assets = KnifeAssets::load(
        KNIFE_PACK,
        ARMS_PACK,
        AvatarSkin::ALL.map(|skin| crate::online_world::agent_atlas(skin.tint())),
        &mut meshes,
        &mut materials,
        &mut images,
    )
    .expect("validated embedded knife and avatar arm assets");

    let mut environment = EnvironmentMapLight::hemispherical_gradient(
        &mut images,
        Color::linear_rgb(0.22, 0.25, 0.29),
        Color::linear_rgb(0.10, 0.11, 0.13),
        Color::linear_rgb(0.045, 0.04, 0.03),
    )
    .with_intensity(1_400.0);
    environment.specular_map = images.add(studio_reflections());

    commands.spawn((
        Camera3d::default(),
        Camera {
            order: 1,
            is_active: false,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        (*target).clone(),
        Projection::Perspective(PerspectiveProjection {
            fov: 66_f32.to_radians(),
            near: 0.03,
            far: 3.0,
            ..default()
        }),
        RenderLayers::layer(KNIFE_LAYER),
        // Direct drawing keeps this overlay's composition working on DX12.
        NoIndirectDrawing,
        Tonemapping::None,
        DebandDither::Disabled,
        environment,
        KnifeCamera,
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 1_600.,
            ..default()
        },
        Transform::default().looking_to(Vec3::new(0.4, -0.55, -1.0), Vec3::Y),
        RenderLayers::layer(KNIFE_LAYER),
    ));
    commands
        .spawn((
            Transform::from_translation(REST_POSITION),
            Visibility::Inherited,
            HeldKnife,
        ))
        .with_children(|parent| {
            for part in assets.shared {
                parent.spawn((
                    Mesh3d(part.mesh),
                    MeshMaterial3d(part.material),
                    RenderLayers::layer(KNIFE_LAYER),
                    NotShadowCaster,
                ));
            }
            for (finish, parts) in KnifeFinish::ALL.into_iter().zip(assets.finishes) {
                let visibility = if finish == KnifeFinish::Classic {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                };
                for part in parts {
                    parent.spawn((
                        Mesh3d(part.mesh),
                        MeshMaterial3d(part.material),
                        RenderLayers::layer(KNIFE_LAYER),
                        NotShadowCaster,
                        visibility,
                        KnifeBlade(finish),
                    ));
                }
            }
            for arm in assets.arms {
                parent.spawn((
                    Mesh3d(arm),
                    MeshMaterial3d(assets.arm_materials[0].clone()),
                    RenderLayers::layer(KNIFE_LAYER),
                    NotShadowCaster,
                    KnifeArm,
                ));
            }
        });
    commands.insert_resource(ArmMaterials(assets.arm_materials));
}

pub(super) fn update(
    mut commands: Commands,
    game: Res<Game>,
    menu: Res<MenuState>,
    online: Res<Online>,
    time: Res<Time<Real>>,
    motion: Res<AccumulatedMouseMotion>,
    arms: Res<ArmMaterials>,
    world_camera: Single<Entity, With<ViewCamera>>,
    knife_camera: Single<(Entity, &mut Camera), With<KnifeCamera>>,
    knife: Single<&mut Transform, With<HeldKnife>>,
    mut blades: Query<(&KnifeBlade, &mut Visibility)>,
    mut arm_meshes: Query<&mut MeshMaterial3d<StandardMaterial>, With<KnifeArm>>,
    mut state: Local<ViewmodelState>,
) {
    let active = menu.page == Page::Playing
        && game.pending.is_none()
        && !game.third_person
        && !game.watching_replay()
        && online.spectating.is_none();
    let (knife_camera, mut camera) = knife_camera.into_inner();
    if camera.is_active != active {
        camera.is_active = active;
        let (previous_ui_camera, next_ui_camera) = if active {
            (*world_camera, knife_camera)
        } else {
            (knife_camera, *world_camera)
        };
        commands
            .entity(previous_ui_camera)
            .remove::<IsDefaultUiCamera>();
        commands.entity(next_ui_camera).insert(IsDefaultUiCamera);
    }
    if !active {
        state.sway = Vec2::ZERO;
        return;
    }

    let loadout = online.equipped_loadout(&menu.settings);
    if state.loadout != Some(loadout) {
        for (blade, mut visibility) in &mut blades {
            *visibility = if blade.0 == loadout.knife_finish {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
        }
        let index = match loadout.avatar_skin {
            AvatarSkin::Classic => 0,
            AvatarSkin::Glacier => 1,
            AvatarSkin::Ember => 2,
        };
        for mut material in &mut arm_meshes {
            material.0 = arms.0[index].clone();
        }
        state.loadout = Some(loadout);
    }

    let shift = Vec2::new(-motion.delta.x, motion.delta.y) * 0.00065;
    let desired = shift.clamp(Vec2::splat(-0.017), Vec2::splat(0.017));
    state.sway = state
        .sway
        .lerp(desired, 1.0 - (-18.0 * time.delta_secs()).exp());
    let breathing = (time.elapsed_secs() * 2.3).sin() * 0.003;
    let mut transform = knife.into_inner();
    transform.translation = REST_POSITION + Vec3::new(state.sway.x, state.sway.y + breathing, 0.0);
    transform.rotation = Quat::from_euler(EulerRot::YXZ, -0.10, 0.12, 0.34 - state.sway.x);
}

pub(crate) fn world_knife_mesh(finish: KnifeFinish) -> Mesh {
    static KNIVES: OnceLock<[Mesh; KnifeFinish::ALL.len()]> = OnceLock::new();
    let meshes = KNIVES.get_or_init(|| {
        knife_asset::load_world_knives(KNIFE_PACK).expect("validated embedded knife world mesh")
    });
    meshes[match finish {
        KnifeFinish::Classic => 0,
        KnifeFinish::Glacier => 1,
        KnifeFinish::Ember => 2,
    }]
    .clone()
}

fn studio_reflections() -> Image {
    const SIDE: u32 = 32;
    const MIPS: u32 = SIDE.ilog2() + 1;
    let mut bytes = Vec::new();
    for mip in 0..MIPS {
        let side = SIDE >> mip;
        let blur = mip as f32 / (MIPS - 1) as f32;
        for face in 0..6 {
            for y in 0..side {
                for x in 0..side {
                    let u = (x as f32 + 0.5) * 2.0 / side as f32 - 1.0;
                    let v = (y as f32 + 0.5) * 2.0 / side as f32 - 1.0;
                    let direction = match face {
                        0 => Vec3::new(1.0, -v, -u),
                        1 => Vec3::new(-1.0, -v, u),
                        2 => Vec3::new(u, 1.0, v),
                        3 => Vec3::new(u, -1.0, -v),
                        4 => Vec3::new(u, -v, 1.0),
                        _ => Vec3::new(-u, -v, -1.0),
                    }
                    .normalize();
                    // Bevy flips the Z axis when sampling an environment cubemap.
                    let world_direction = Vec3::new(direction.x, direction.y, -direction.z);
                    bytes.extend(reflection_pixel(world_direction, blur));
                }
            }
        }
    }
    let mut image = Image::new_uninit(
        Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 6,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.mip_level_count = MIPS;
    image.data_order = TextureDataOrder::MipMajor;
    image.data = Some(bytes);
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..default()
    });
    image.sampler = ImageSampler::linear();
    image
}

fn reflection_pixel(direction: Vec3, blur: f32) -> [u8; 4] {
    let mut light = Vec3::new(0.16, 0.17, 0.19) * (1.0 + direction.y * 0.38);
    for (axis, horizontal, center, size, color) in [
        (
            Vec3::Z,
            Vec3::X,
            Vec2::new(-0.14, -0.36),
            Vec2::new(0.49, 0.43),
            Vec3::new(0.56, 0.61, 0.68),
        ),
        (
            Vec3::NEG_X,
            Vec3::Z,
            Vec2::new(0.67, -0.90),
            Vec2::new(0.54, 0.72),
            Vec3::new(0.86, 0.90, 0.96),
        ),
        (
            Vec3::X,
            Vec3::Z,
            Vec2::new(0.42, -0.10),
            Vec2::new(0.23, 0.55),
            Vec3::new(0.55, 0.43, 0.31),
        ),
    ] {
        let facing = direction.dot(axis);
        if facing <= 0.06 {
            continue;
        }
        let position = Vec2::new(direction.dot(horizontal), direction.y) / facing;
        let edge = (position - center).abs() - size * (1.0 + blur * 0.8);
        let alpha = (0.5 - edge.max_element() / (0.10 + blur)).clamp(0.0, 1.0);
        light += color * (alpha * alpha * (3.0 - 2.0 * alpha)) * (1.0 - 0.62 * blur);
    }
    let light = light.min(Vec3::ONE);
    Color::linear_rgb(light.x, light.y, light.z)
        .to_srgba()
        .to_u8_array()
}
