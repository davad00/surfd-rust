use bevy::asset::RenderAssetUsages;
use bevy::camera::{RenderTarget, visibility::RenderLayers};
use bevy::color::ColorToComponents;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::light::NotShadowCaster;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;
use bevy::render::view::{DebandDither, NoIndirectDrawing, Tonemapping};

use crate::cosmetics::KnifeFinish;
use crate::menu::{MenuState, Page};
use crate::online::Online;

use super::{Game, ViewCamera};

const KNIFE_LAYER: usize = 2;
const REST_POSITION: Vec3 = Vec3::new(0.295, -0.235, -0.73);

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
    transform.rotation = Quat::from_euler(EulerRot::YXZ, -0.15, 0.13, 0.27 - sway.x);
}

fn knife_mesh(finish: KnifeFinish) -> Mesh {
    let mut model = knife_model(finish);
    let wrist = Vec3::new(0.046, -0.137, -0.013);
    let cuff = Vec3::new(0.074, -0.178, -0.018);
    model.taper(
        wrist,
        cuff,
        Vec2::new(0.050, 0.038),
        Vec2::new(0.054, 0.041),
        [43, 52, 61],
    );
    model.taper(
        cuff,
        Vec3::new(0.227, -0.422, 0.016),
        Vec2::new(0.054, 0.041),
        Vec2::new(0.085, 0.065),
        [68, 79, 88],
    );
    model.ellipsoid(
        Vec3::new(0.036, -0.091, 0.025),
        Vec3::new(0.057, 0.067, 0.041),
        [40, 46, 52],
    );
    for finger in 0..4 {
        let offset = finger as f32;
        model.ellipsoid(
            Vec3::new(0.005 + offset * 0.006, -0.045 - offset * 0.026, 0.045),
            Vec3::new(0.057 - offset * 0.003, 0.014, 0.029),
            [52, 60, 68],
        );
    }
    model.ellipsoid(
        Vec3::new(0.058, -0.035, 0.034),
        Vec3::new(0.022, 0.035, 0.026),
        [48, 55, 63],
    );
    model.finish()
}

pub(crate) fn world_knife_mesh(finish: KnifeFinish) -> Mesh {
    knife_model(finish).finish()
}

fn knife_model(finish: KnifeFinish) -> Model {
    let palette = finish.palette();
    let mut model = Model::default();
    let profile = [
        Vec2::new(-0.049, 0.021),
        Vec2::new(0.045, 0.021),
        Vec2::new(0.053, 0.096),
        Vec2::new(0.044, 0.169),
        Vec2::new(0.018, 0.233),
        Vec2::new(-0.025, 0.305),
        Vec2::new(-0.045, 0.229),
        Vec2::new(-0.050, 0.113),
    ];
    let inset = Vec2::new(-0.006, 0.147);
    for face in [1.0, -1.0] {
        let mut profile = profile;
        if face < 0.0 {
            profile.reverse();
        }
        let outer = profile.map(|point| point.extend(0.0));
        let inner = profile.map(|point| point.lerp(inset, 0.19).extend(face * 0.011));
        for index in 0..outer.len() {
            let next = (index + 1) % outer.len();
            model.quad(
                [outer[index], outer[next], inner[next], inner[index]],
                palette.bevel,
            );
            model.triangle(
                [inset.extend(face * 0.011), inner[index], inner[next]],
                palette.blade,
            );
        }
    }
    model.quad(
        [
            Vec3::new(-0.028, 0.068, 0.0112),
            Vec3::new(-0.016, 0.061, 0.0112),
            Vec3::new(-0.011, 0.210, 0.0112),
            Vec3::new(-0.023, 0.226, 0.0112),
        ],
        palette.handle,
    );
    model.taper(
        -Vec3::Y * 0.179,
        Vec3::Y * 0.019,
        Vec2::new(0.026, 0.023),
        Vec2::new(0.032, 0.028),
        palette.handle,
    );
    model.taper(
        Vec3::Y * 0.001,
        Vec3::Y * 0.023,
        Vec2::new(0.069, 0.028),
        Vec2::new(0.076, 0.031),
        palette.handle,
    );
    model.taper(
        Vec3::Y * 0.021,
        Vec3::Y * 0.029,
        Vec2::new(0.072, 0.031),
        Vec2::new(0.069, 0.025),
        palette.grip,
    );
    model.taper(
        -Vec3::Y * 0.184,
        -Vec3::Y * 0.163,
        Vec2::new(0.031, 0.024),
        Vec2::new(0.033, 0.027),
        palette.grip,
    );

    model
}

#[derive(Default)]
struct Model {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
}

impl Model {
    fn triangle(&mut self, points: [Vec3; 3], [red, green, blue]: [u8; 3]) {
        let Some(normal) = (points[1] - points[0])
            .cross(points[2] - points[0])
            .try_normalize()
        else {
            return;
        };
        let light = Vec3::new(-0.37, 0.73, 0.56).normalize();
        let shade = 0.54 + 0.46 * normal.dot(light).max(0.0);
        let mut color = Color::srgb_u8(red, green, blue).to_linear().to_f32_array();
        for channel in &mut color[..3] {
            *channel *= shade;
        }
        self.positions.extend(points.map(|point| point.to_array()));
        self.normals.extend([normal.to_array(); 3]);
        self.colors.extend([color; 3]);
    }

    fn quad(&mut self, [a, b, c, d]: [Vec3; 4], color: [u8; 3]) {
        self.triangle([a, b, c], color);
        self.triangle([a, c, d], color);
    }

    fn taper(&mut self, start: Vec3, end: Vec3, first: Vec2, last: Vec2, color: [u8; 3]) {
        const SIDES: usize = 8;
        let axis = (end - start).normalize();
        let right = Vec3::Z.cross(axis).normalize();
        let depth = axis.cross(right);
        let ring = |center, size: Vec2, segment| {
            let angle = std::f32::consts::TAU * segment as f32 / SIDES as f32;
            center + right * size.x * angle.cos() + depth * size.y * angle.sin()
        };
        for side in 0..SIDES {
            let a = ring(start, first, side);
            let b = ring(start, first, side + 1);
            let c = ring(end, last, side + 1);
            let d = ring(end, last, side);
            self.quad([a, b, c, d], color);
            self.triangle([start, b, a], color);
            self.triangle([end, d, c], color);
        }
    }

    fn ellipsoid(&mut self, center: Vec3, radius: Vec3, color: [u8; 3]) {
        const SIDES: usize = 8;
        const LEVELS: usize = 5;
        let vertex = |segment, level| {
            let longitude = std::f32::consts::TAU * segment as f32 / SIDES as f32;
            let latitude = std::f32::consts::PI * (level as f32 / LEVELS as f32 - 0.5);
            center
                + radius
                    * Vec3::new(
                        longitude.sin() * latitude.cos(),
                        latitude.sin(),
                        longitude.cos() * latitude.cos(),
                    )
        };
        for level in 0..LEVELS {
            for side in 0..SIDES {
                self.quad(
                    [
                        vertex(side, level),
                        vertex(side + 1, level),
                        vertex(side + 1, level + 1),
                        vertex(side, level + 1),
                    ],
                    color,
                );
            }
        }
    }

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
