use crate::{
    cosmetics::AvatarSkin,
    menu::MenuState,
    online::Online,
    view::{Game, ViewCamera},
};
use bevy::asset::{RenderAssetUsages, embedded_asset};
use bevy::image::ImageSampler;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, TextureDimension, TextureFormat};
use bevy::shader::ShaderRef;
use std::collections::{BTreeMap, HashSet};
use std::f32::consts::PI;

#[derive(Component)]
pub struct RemoteAvatar;

#[derive(Default)]
pub struct Actors {
    entities: BTreeMap<u64, Entity>,
    live: HashSet<u64>,
    local: Option<Entity>,
    ghost: Option<Entity>,
    handles: Option<AgentAssets>,
    skins: [Option<Handle<StandardMaterial>>; 3],
    version: Option<u64>,
}

#[derive(Clone)]
struct AgentAssets {
    standing: Handle<Mesh>,
    crouching: Handle<Mesh>,
    local: Handle<StandardMaterial>,
    remote: Handle<RemoteMaterial>,
    ghost: Handle<GhostMaterial>,
}

impl AgentAssets {
    fn mesh(&self, crouched: bool) -> Mesh3d {
        Mesh3d(if crouched {
            self.crouching.clone()
        } else {
            self.standing.clone()
        })
    }
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct GhostMaterial {
    #[uniform(0)]
    opacity: f32,
    #[texture(1)]
    #[sampler(2)]
    atlas: Handle<Image>,
}

impl Material for GhostMaterial {
    fn fragment_shader() -> ShaderRef {
        "embedded://bevy_surf/surfd_ghost.wesl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn enable_shadows() -> bool {
        false
    }
}

pub type RemoteMaterial = ExtendedMaterial<StandardMaterial, RemoteFade>;

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct RemoteFade {}

impl MaterialExtension for RemoteFade {
    fn vertex_shader() -> ShaderRef {
        "embedded://bevy_surf/surfd_avatar.wesl".into()
    }
}

pub fn register_avatar_materials(app: &mut App) {
    embedded_asset!(app, "surfd_ghost.wesl");
    embedded_asset!(app, "surfd_avatar.wesl");
    app.add_plugins((
        MaterialPlugin::<GhostMaterial>::default(),
        MaterialPlugin::<RemoteMaterial>::default(),
    ));
}

pub fn update_players(
    mut commands: Commands,
    online: Res<Online>,
    game: Res<Game>,
    menu: Res<MenuState>,
    camera: Single<&Transform, (With<ViewCamera>, Without<RemoteAvatar>)>,
    mut assets: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut ghost_materials: ResMut<Assets<GhostMaterial>>,
    mut remote_materials: ResMut<Assets<RemoteMaterial>>,
    mut avatars: Query<(&mut Transform, &mut Mesh3d, &mut Visibility), With<RemoteAvatar>>,
    mut local_materials: Query<&mut MeshMaterial3d<StandardMaterial>, With<RemoteAvatar>>,
    mut actors: Local<Actors>,
) {
    if actors.version != Some(game.course_version) {
        for (_, entity) in std::mem::take(&mut actors.entities) {
            commands.entity(entity).despawn();
        }
        if let Some(entity) = actors.ghost.take() {
            commands.entity(entity).despawn();
        }
        if let Some(entity) = actors.local.take() {
            commands.entity(entity).despawn();
        }
        actors.version = Some(game.course_version);
    }
    let local = game
        .third_person_pose()
        .filter(|_| online.viewed_player().is_none());
    let ghost = game.ghost_pose().filter(|_| !game.watching_replay());
    let show_players = online.show_players(&game);
    if !show_players
        && local.is_none()
        && ghost.is_none()
        && actors.local.is_none()
        && actors.ghost.is_none()
        && actors.entities.is_empty()
    {
        return;
    }
    let agent = actors
        .handles
        .get_or_insert_with(|| {
            load_agent(
                &mut assets,
                &mut images,
                &mut materials,
                &mut ghost_materials,
                &mut remote_materials,
            )
        })
        .clone();
    if let Some((position, yaw, crouched)) = local {
        let eye = position + Vec3::Y * if crouched { 46.0 } else { 64.0 };
        let visible = if camera.translation.distance_squared(eye) > 28.0 * 28.0 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        let pose =
            Transform::from_translation(position).with_rotation(Quat::from_rotation_y(yaw + PI));
        let mesh = agent.mesh(crouched);
        let skin = menu.settings.avatar_skin;
        let material = if let Some(tint) = skin.tint() {
            let index = AvatarSkin::ALL
                .iter()
                .position(|&choice| choice == skin)
                .expect("registered player skin");
            actors.skins[index]
                .get_or_insert_with(|| {
                    let atlas = images.add(agent_atlas(Some(tint)));
                    materials.add(player_material(atlas))
                })
                .clone()
        } else {
            agent.local.clone()
        };
        if let Some(entity) = actors.local {
            if let Ok((mut transform, mut current_mesh, mut visibility)) = avatars.get_mut(entity) {
                transform.set_if_neq(pose);
                current_mesh.set_if_neq(mesh);
                visibility.set_if_neq(visible);
            }
            if let Ok(mut equipped) = local_materials.get_mut(entity)
                && equipped.0 != material
            {
                equipped.0 = material;
            }
        } else {
            actors.local = Some(
                commands
                    .spawn((RemoteAvatar, mesh, MeshMaterial3d(material), pose, visible))
                    .id(),
            );
        }
    } else if let Some(entity) = actors.local.take() {
        commands.entity(entity).despawn();
    }
    actors.live.clear();
    if show_players {
        for player in online.players.iter().filter(|player| {
            Some(player.id) != online.id
                && Some(player.id) != online.spectating
                && player.spectating.is_none()
        }) {
            let Some(state) = player.state else {
                continue;
            };
            let state = online.displayed_pose(player.id, state);
            actors.live.insert(player.id);
            let target = Vec3::from_array(state.position);
            let rotation = Quat::from_rotation_y(state.yaw + PI);
            if let Some(&entity) = actors.entities.get(&player.id) {
                if let Ok((mut transform, mut mesh, _)) = avatars.get_mut(entity) {
                    transform
                        .set_if_neq(Transform::from_translation(target).with_rotation(rotation));
                    mesh.set_if_neq(agent.mesh(state.crouched));
                }
            } else {
                let entity = commands
                    .spawn((
                        RemoteAvatar,
                        agent.mesh(state.crouched),
                        MeshMaterial3d(agent.remote.clone()),
                        Transform::from_translation(target).with_rotation(rotation),
                        Visibility::Inherited,
                    ))
                    .id();
                actors.entities.insert(player.id, entity);
            }
        }
    }
    let Actors { entities, live, .. } = &mut *actors;
    entities.retain(|id, entity| {
        if live.contains(id) {
            true
        } else {
            commands.entity(*entity).despawn();
            false
        }
    });

    if let Some((position, yaw, crouched)) = ghost {
        let height = if crouched { 53.36 } else { 72.0 };
        let distance = camera
            .translation
            .distance(position + Vec3::Y * (height / 2.0));
        let opacity = 0.45
            * smooth_step(36.0, 136.0, distance)
            * (1.0 + 0.45 * smooth_step(400.0, 1600.0, distance));
        if ghost_materials
            .get(&agent.ghost)
            .is_some_and(|material| material.opacity != opacity)
            && let Some(mut material) = ghost_materials.get_mut(&agent.ghost)
        {
            material.opacity = opacity;
        }
        let pose =
            Transform::from_translation(position).with_rotation(Quat::from_rotation_y(yaw + PI));
        let mesh = agent.mesh(crouched);
        if let Some(entity) = actors.ghost {
            if let Ok((mut transform, mut current_mesh, mut visibility)) = avatars.get_mut(entity) {
                transform.set_if_neq(pose);
                current_mesh.set_if_neq(mesh);
                visibility.set_if_neq(avatar_visibility(opacity));
            }
        } else {
            actors.ghost = Some(
                commands
                    .spawn((
                        RemoteAvatar,
                        mesh,
                        MeshMaterial3d(agent.ghost),
                        pose,
                        avatar_visibility(opacity),
                    ))
                    .id(),
            );
        }
    } else if let Some(entity) = actors.ghost.take() {
        commands.entity(entity).despawn();
    }
}

fn avatar_visibility(opacity: f32) -> Visibility {
    if opacity > 0.01 {
        Visibility::Visible
    } else {
        Visibility::Hidden
    }
}

fn smooth_step(start: f32, end: f32, value: f32) -> f32 {
    let t = ((value - start) / (end - start)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn load_agent(
    meshes: &mut Assets<Mesh>,
    images: &mut Assets<Image>,
    materials: &mut Assets<StandardMaterial>,
    ghost_materials: &mut Assets<GhostMaterial>,
    remote_materials: &mut Assets<RemoteMaterial>,
) -> AgentAssets {
    let atlas = images.add(agent_atlas(None));
    AgentAssets {
        standing: meshes.add(agent_mesh(false)),
        crouching: meshes.add(agent_mesh(true)),
        local: materials.add(player_material(atlas.clone())),
        remote: remote_materials.add(RemoteMaterial {
            base: StandardMaterial {
                base_color_texture: Some(atlas.clone()),
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                fog_enabled: false,
                ..default()
            },
            extension: RemoteFade {},
        }),
        ghost: ghost_materials.add(GhostMaterial {
            opacity: 0.45,
            atlas,
        }),
    }
}

fn player_material(atlas: Handle<Image>) -> StandardMaterial {
    StandardMaterial {
        base_color_texture: Some(atlas),
        unlit: true,
        fog_enabled: false,
        ..default()
    }
}

fn agent_atlas(tint: Option<[u8; 3]>) -> Image {
    let mut atlas = image::load_from_memory(include_bytes!("../assets/agent/atlas.png"))
        .expect("embedded Surfd agent atlas")
        .into_rgba8();
    if let Some(tint) = tint {
        recolor_clothing(&mut atlas, tint);
    }
    let mut image = Image::new(
        Extent3d {
            width: atlas.width(),
            height: atlas.height(),
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        atlas.into_raw(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::linear();
    image
}

fn recolor_clothing(atlas: &mut image::RgbaImage, tint: [u8; 3]) {
    // Atlas coordinates for the jacket, pants, and sleeve. Face, hands, hair,
    // eyes, and shoes retain their original pixels.
    for (x, y, pixel) in atlas.enumerate_pixels_mut() {
        let body = x < 128 && y < 128;
        let sleeve = (96..128).contains(&x) && (128..216).contains(&y);
        if !body && !sleeve {
            continue;
        }
        let [r, g, b, _] = pixel.0;
        let brightest = r.max(g).max(b);
        if brightest > 128 || brightest - r.min(g).min(b) > 18 {
            continue;
        }
        let light = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
        for (channel, color) in pixel.0[..3].iter_mut().zip(tint) {
            *channel = ((u16::from(color) * (light + 24) / 105).min(255)) as u8;
        }
    }
}

#[cfg(test)]
mod skin_tests {
    use super::*;

    #[test]
    fn skins_change_clothing_without_recoloring_the_face_hands_or_eyes() {
        let original = image::load_from_memory(include_bytes!("../assets/agent/atlas.png"))
            .unwrap()
            .into_rgba8();
        let mut glacier = original.clone();
        let mut ember = original.clone();
        recolor_clothing(&mut glacier, AvatarSkin::Glacier.tint().unwrap());
        recolor_clothing(&mut ember, AvatarSkin::Ember.tint().unwrap());
        for (x, y) in [(150, 90), (32, 236), (110, 230), (30, 159)] {
            assert_eq!(glacier.get_pixel(x, y), original.get_pixel(x, y));
            assert_eq!(ember.get_pixel(x, y), original.get_pixel(x, y));
        }
        assert_ne!(glacier.get_pixel(80, 60), original.get_pixel(80, 60));
        assert_ne!(glacier.get_pixel(80, 60), ember.get_pixel(80, 60));
    }
}

fn agent_mesh(crouching: bool) -> Mesh {
    // Original Surfd agent mesh. Counts are followed by two XYZ poses in
    // hundredths of a unit, normalized UV/color, then triangle indices.
    let bytes = include_bytes!("../assets/agent/mesh.bin");
    let word = |offset| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
    let count = usize::from(word(0));
    let index_count = usize::from(word(2));
    let indices = 4 + count * 19;
    let mut positions = Vec::with_capacity(count);
    let mut uv = Vec::with_capacity(count);
    let mut colors = Vec::with_capacity(count);
    for i in 0..count {
        let start = 4 + i * 19;
        let position = start + if crouching { 6 } else { 0 };
        positions.push(std::array::from_fn::<_, 3, _>(|axis| {
            f32::from(word(position + axis * 2) as i16) / 100.0
        }));
        uv.push([
            f32::from(word(start + 12)) / 65535.0,
            1.0 - f32::from(word(start + 14)) / 65535.0,
        ]);
        let color = |channel: usize| f32::from(bytes[start + 16 + channel]) / 255.0;
        colors.push([color(0), color(1), color(2), 1.0]);
    }
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
    .with_inserted_indices(Indices::U16(
        (0..index_count).map(|i| word(indices + i * 2)).collect(),
    ));
    mesh.compute_smooth_normals();
    mesh
}
