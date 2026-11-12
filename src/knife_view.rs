use bevy::asset::RenderAssetUsages;
use bevy::camera::{RenderTarget, visibility::RenderLayers};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::light::NotShadowCaster;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;
use bevy::render::view::{DebandDither, NoIndirectDrawing, Tonemapping};

use crate::cosmetics::KnifeFinish;
use crate::menu::{MenuState, Page};
use crate::online::Online;

use super::{Game, ViewCamera};

#[path = "knife_hand.rs"]
mod knife_hand;
#[path = "knife_model.rs"]
mod knife_model;

const KNIFE_LAYER: usize = 2;
const REST_POSITION: Vec3 = Vec3::new(0.310, -0.235, -0.76);

#[derive(Component)]
pub(super) struct KnifeCamera;

#[derive(Component)]
pub(super) struct HeldKnife;

#[derive(Resource)]
pub(super) struct KnifeMeshes(Vec<(KnifeFinish, Handle<Mesh>)>);

pub(super) fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    target: Single<&RenderTarget, With<ViewCamera>>,
) {
    let finishes: Vec<_> = KnifeFinish::ALL
        .into_iter()
        .map(|finish| (finish, meshes.add(knife_mesh(finish))))
        .collect();
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
        // Direct drawing for this single mesh keeps DX12 camera composition working.
        NoIndirectDrawing,
        Tonemapping::None,
        DebandDither::Disabled,
        KnifeCamera,
    ));
    commands.spawn((
        Mesh3d(finishes[0].1.clone()),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::WHITE,
            unlit: true,
            cull_mode: None,
            ..default()
        })),
        Transform::from_translation(REST_POSITION),
        RenderLayers::layer(KNIFE_LAYER),
        NotShadowCaster,
        HeldKnife,
    ));
    commands.insert_resource(KnifeMeshes(finishes));
}

pub(super) fn update(
    mut commands: Commands,
    game: Res<Game>,
    menu: Res<MenuState>,
    online: Res<Online>,
    time: Res<Time<Real>>,
    motion: Res<AccumulatedMouseMotion>,
    meshes: Res<KnifeMeshes>,
    world_camera: Single<Entity, With<ViewCamera>>,
    knife_camera: Single<(Entity, &mut Camera), With<KnifeCamera>>,
    knife: Single<(&mut Transform, &mut Mesh3d), With<HeldKnife>>,
    mut sway: Local<Vec2>,
) {
    let active = menu.page == Page::Playing
        && game.pending.is_none()
        && !game.third_person
        && !game.watching_replay()
        && online.spectating.is_none();
    let (knife_camera, mut camera) = knife_camera.into_inner();
    if camera.is_active != active {
        camera.is_active = active;
        // Render the HUD on the last active camera so it stays above the knife.
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
        *sway = Vec2::ZERO;
        return;
    }

    let (mut transform, mut mesh) = knife.into_inner();
    let selected_finish = online.equipped_loadout(&menu.settings).knife_finish;
    if let Some((_, selected)) = meshes
        .0
        .iter()
        .find(|(finish, _)| *finish == selected_finish)
        && mesh.0 != *selected
    {
        mesh.0 = selected.clone();
    }

    let shift = Vec2::new(-motion.delta.x, motion.delta.y) * 0.00065;
    let desired = shift.clamp(Vec2::splat(-0.017), Vec2::splat(0.017));
    *sway = sway.lerp(desired, 1.0 - (-18.0 * time.delta_secs()).exp());
    let breathing = (time.elapsed_secs() * 2.3).sin() * 0.003;
    transform.translation = REST_POSITION + Vec3::new(sway.x, sway.y + breathing, 0.0);
    transform.rotation = Quat::from_euler(EulerRot::YXZ, -0.24, 0.12, 0.45 - sway.x);
}

fn knife_mesh(finish: KnifeFinish) -> Mesh {
    let mut model = knife_model::build(finish);
    knife_hand::add_hand(&mut model);
    model.finish()
}

pub(crate) fn world_knife_mesh(finish: KnifeFinish) -> Mesh {
    knife_model::build(finish).finish()
}

#[derive(Default)]
struct Model {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
}

impl Model {
    fn finish(self) -> Mesh {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, self.colors)
    }
}
