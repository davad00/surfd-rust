use crate::{
    cosmetics::{AvatarSkin, KnifeFinish, Loadout},
    menu::MenuState,
    online::Online,
    view::{Game, ViewCamera, knife_view::world_knife_mesh},
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
    entities: BTreeMap<u64, RemoteActor>,
    live: HashSet<u64>,
    local: Option<Entity>,
    ghost: Option<Entity>,
    handles: Option<AgentAssets>,
    version: Option<u64>,
}

struct RemoteActor {
    entity: Entity,
    crouched: bool,
    loadout: Loadout,
}

struct AgentAssets {
    standing: Handle<Mesh>,
    crouching: Handle<Mesh>,
    equipped: [[Option<Handle<Mesh>>; 2]; KnifeFinish::ALL.len()],
    skins: [Option<AgentSkin>; AvatarSkin::ALL.len()],
    ghost: Handle<GhostMaterial>,
}

struct AgentSkin {
    local: Handle<StandardMaterial>,
    remote: Handle<RemoteMaterial>,
}

impl AgentAssets {
    fn mesh(&self, crouched: bool) -> Mesh3d {
        Mesh3d(if crouched {
            self.crouching.clone()
        } else {
            self.standing.clone()
        })
    }

    fn equipped_mesh(
        &mut self,
        crouched: bool,
        finish: KnifeFinish,
        meshes: &mut Assets<Mesh>,
    ) -> Mesh3d {
        let index = KnifeFinish::ALL
            .iter()
            .position(|&choice| choice == finish)
            .expect("registered knife finish");
        let mesh = self.equipped[index][usize::from(crouched)]
            .get_or_insert_with(|| meshes.add(equipped_agent_mesh(crouched, finish)));
        Mesh3d(mesh.clone())
    }

    fn skin(
        &mut self,
        skin: AvatarSkin,
        images: &mut Assets<Image>,
        materials: &mut Assets<StandardMaterial>,
        remote_materials: &mut Assets<RemoteMaterial>,
    ) -> &AgentSkin {
        let index = AvatarSkin::ALL
            .iter()
            .position(|&choice| choice == skin)
            .expect("registered player skin");
        self.skins[index].get_or_insert_with(|| {
            let atlas = images.add(agent_atlas(skin.tint()));
            add_agent_skin(atlas, materials, remote_materials)
        })
    }
}

// A white sample in the unused atlas corner lets the knife use vertex colors
// in the same material and draw as the avatar.
const KNIFE_ATLAS_UV: [f32; 2] = [254.0 / 256.0; 2];

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
    mut remote_looks: Query<&mut MeshMaterial3d<RemoteMaterial>, With<RemoteAvatar>>,
    mut actors: Local<Actors>,
) {
    let actors = &mut *actors;
    if actors.version != Some(game.course_version) {
        for (_, actor) in std::mem::take(&mut actors.entities) {
            commands.entity(actor.entity).despawn();
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
    let agent = actors.handles.get_or_insert_with(|| {
        load_agent(
            &mut assets,
            &mut images,
            &mut materials,
            &mut ghost_materials,
            &mut remote_materials,
        )
    });
    if let Some((position, yaw, crouched)) = local {
        let eye = position + Vec3::Y * if crouched { 46.0 } else { 64.0 };
        let visible = if camera.translation.distance_squared(eye) > 28.0 * 28.0 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        let pose =
            Transform::from_translation(position).with_rotation(Quat::from_rotation_y(yaw + PI));
        let loadout = online.equipped_loadout(&menu.settings);
        let mesh = if game.watching_replay() {
            agent.mesh(crouched)
        } else {
            agent.equipped_mesh(crouched, loadout.knife_finish, &mut assets)
        };
        let material = agent
            .skin(
                loadout.avatar_skin,
                &mut images,
                &mut materials,
                &mut remote_materials,
            )
            .local
            .clone();
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
            let pose = Transform::from_translation(target).with_rotation(rotation);
            let actor = actors.entities.get_mut(&player.id);
            let current_mesh = match actor.as_ref() {
                Some(actor) => {
                    let Ok((mut transform, current_mesh, _)) = avatars.get_mut(actor.entity) else {
                        continue;
                    };
                    transform.set_if_neq(pose);
                    if actor.crouched == state.crouched && actor.loadout == player.loadout {
                        continue;
                    }
                    Some(current_mesh)
                }
                None => None,
            };
            let mesh =
                agent.equipped_mesh(state.crouched, player.loadout.knife_finish, &mut assets);
            let material = agent
                .skin(
                    player.loadout.avatar_skin,
                    &mut images,
                    &mut materials,
                    &mut remote_materials,
                )
                .remote
                .clone();
            if let Some(actor) = actor {
                if let Some(mut current_mesh) = current_mesh {
                    current_mesh.set_if_neq(mesh);
                }
                if let Ok(mut equipped) = remote_looks.get_mut(actor.entity)
                    && equipped.0 != material
                {
                    equipped.0 = material;
                }
                actor.crouched = state.crouched;
                actor.loadout = player.loadout;
            } else {
                let entity = commands
                    .spawn((
                        RemoteAvatar,
                        mesh,
                        MeshMaterial3d(material),
                        pose,
                        Visibility::Inherited,
                    ))
                    .id();
                actors.entities.insert(
                    player.id,
                    RemoteActor {
                        entity,
                        crouched: state.crouched,
                        loadout: player.loadout,
                    },
                );
            }
        }
    }
    actors.entities.retain(|id, actor| {
        if actors.live.contains(id) {
            true
        } else {
            commands.entity(actor.entity).despawn();
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
                        MeshMaterial3d(agent.ghost.clone()),
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
        equipped: Default::default(),
        skins: [
            Some(add_agent_skin(atlas.clone(), materials, remote_materials)),
            None,
            None,
        ],
        ghost: ghost_materials.add(GhostMaterial {
            opacity: 0.45,
            atlas,
        }),
    }
}

fn add_agent_skin(
    atlas: Handle<Image>,
    materials: &mut Assets<StandardMaterial>,
    remote_materials: &mut Assets<RemoteMaterial>,
) -> AgentSkin {
    AgentSkin {
        local: materials.add(player_material(atlas.clone())),
        remote: remote_materials.add(RemoteMaterial {
            base: StandardMaterial {
                base_color_texture: Some(atlas),
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                fog_enabled: false,
                ..default()
            },
            extension: RemoteFade {},
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

pub(crate) fn agent_atlas(tint: Option<[u8; 3]>) -> Image {
    let mut atlas = image::load_from_memory(include_bytes!("../assets/agent/atlas.png"))
        .expect("embedded Surfd agent atlas")
        .into_rgba8();
    if let Some(tint) = tint {
        recolor_clothing(&mut atlas, tint);
    }
    for y in 252..256 {
        for x in 252..256 {
            atlas.put_pixel(x, y, image::Rgba([255; 4]));
        }
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
    use bevy::mesh::VertexAttributeValues;

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

    #[test]
    fn equipped_avatar_keeps_body_triangles_and_adds_the_selected_knife_colors() {
        for crouched in [false, true] {
            let plain = agent_mesh(crouched);
            let glacier = equipped_agent_mesh(crouched, KnifeFinish::Glacier);
            let ember = equipped_agent_mesh(crouched, KnifeFinish::Ember);
            let original_vertices = plain.count_vertices();
            assert!(glacier.count_vertices() > original_vertices);
            assert_eq!(glacier.count_vertices(), ember.count_vertices());

            let original_indices = plain.indices().unwrap();
            let equipped_indices = glacier.indices().unwrap();
            assert!(
                equipped_indices
                    .iter()
                    .take(original_indices.len())
                    .eq(original_indices.iter())
            );
            assert!(
                equipped_indices
                    .iter()
                    .skip(original_indices.len())
                    .eq(original_vertices..glacier.count_vertices())
            );
            let Some(VertexAttributeValues::Float32x2(uv)) =
                glacier.attribute(Mesh::ATTRIBUTE_UV_0)
            else {
                panic!("equipped avatar needs UV coordinates");
            };
            assert_eq!(uv.len(), glacier.count_vertices());
            assert!(
                uv[original_vertices..]
                    .iter()
                    .all(|&uv| uv == KNIFE_ATLAS_UV)
            );
            assert_ne!(
                glacier.attribute(Mesh::ATTRIBUTE_COLOR),
                ember.attribute(Mesh::ATTRIBUTE_COLOR)
            );
        }
    }
}

fn agent_mesh(crouching: bool) -> Mesh {
    // Baked avatar mesh. Counts are followed by two XYZ poses in
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

fn equipped_agent_mesh(crouching: bool, finish: KnifeFinish) -> Mesh {
    // Pivots are the center of the avatar's right hand in its two baked poses.
    let grip = if crouching {
        Vec3::new(-19.0, 16.7, -6.0)
    } else {
        Vec3::new(-19.0, 35.0, 2.1)
    };
    let rotation = Quat::from_euler(EulerRot::YXZ, -0.14, 1.12, 0.62);
    let scale = 34.0;
    let pose = Transform {
        translation: grip + rotation * Vec3::Y * (0.1 * scale),
        rotation,
        scale: Vec3::splat(scale),
    };
    let mut knife = world_knife_mesh(finish).transformed_by(pose);
    let count = knife.count_vertices();
    knife.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![KNIFE_ATLAS_UV; count]);
    knife.insert_indices(Indices::U32((0..count as u32).collect()));
    let mut avatar = agent_mesh(crouching);
    avatar
        .merge(&knife)
        .expect("compatible avatar and knife geometry");
    avatar
}
