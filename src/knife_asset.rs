use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::color::ColorToComponents;
use bevy::image::ImageSampler;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, Face, TextureDimension, TextureFormat};
use image::RgbaImage;
use image::imageops::{FilterType, resize};
use serde::Deserialize;

const FINISH_COUNT: usize = 3;
const SHARED_GROUPS: std::ops::Range<usize> = FINISH_COUNT..5;
const VERTEX_BYTES: usize = 8 * size_of::<f32>();

pub(crate) struct KnifePart {
    pub(crate) mesh: Handle<Mesh>,
    pub(crate) material: Handle<StandardMaterial>,
}

pub(crate) struct KnifeAssets {
    pub(crate) shared: Vec<KnifePart>,
    pub(crate) finishes: [Vec<KnifePart>; FINISH_COUNT],
    pub(crate) arms: Vec<Handle<Mesh>>,
    pub(crate) arm_materials: [Handle<StandardMaterial>; 3],
}

impl KnifeAssets {
    pub(crate) fn load(
        knife_pack: &[u8],
        arms_pack: &[u8],
        avatar_atlases: [Image; 3],
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
    ) -> Result<Self, String> {
        let knife = Pack::read(knife_pack, "knife")?;
        let arms = Pack::read(arms_pack, "arms")?;

        let mut needs_tangents = vec![false; knife.manifest.meshes.len()];
        for part in &knife.manifest.parts {
            needs_tangents[part.mesh] |= knife.material(part).normal_map.is_some();
        }
        let knife_meshes = knife
            .geometry()?
            .into_iter()
            .zip(needs_tangents)
            .map(|(geometry, tangents)| geometry.into_mesh(tangents))
            .collect::<Result<Vec<_>, _>>()?;
        let arms_meshes = arms
            .geometry()?
            .into_iter()
            .map(|geometry| geometry.into_mesh(false))
            .collect::<Result<Vec<_>, _>>()?;
        let pixels = (0..knife.manifest.textures.len())
            .map(|index| knife.decode_texture(index))
            .collect::<Result<Vec<_>, _>>()?;

        let mut texture_handles = TextureHandles {
            pixels: &pixels,
            cache: HashMap::new(),
            images,
        };
        let knife_materials: Vec<_> = knife
            .manifest
            .materials
            .iter()
            .map(|source| materials.add(source.to_material(&mut texture_handles)))
            .collect();
        let knife_meshes: Vec<_> = knife_meshes
            .into_iter()
            .map(|mesh| meshes.add(mesh))
            .collect();
        let arms_meshes: Vec<_> = arms_meshes
            .into_iter()
            .map(|mesh| meshes.add(mesh))
            .collect();

        let mut shared = Vec::new();
        let mut finishes = std::array::from_fn(|_| Vec::new());
        for part in knife.manifest.parts {
            let item = KnifePart {
                mesh: knife_meshes[part.mesh].clone(),
                material: knife_materials[part.material.expect("validated knife material")].clone(),
            };
            if let Some(finish) = finishes.get_mut(part.group) {
                finish.push(item);
            } else {
                shared.push(item);
            }
        }
        let arms = arms
            .manifest
            .parts
            .into_iter()
            .map(|part| arms_meshes[part.mesh].clone())
            .collect();
        let arm_materials = avatar_atlases.map(|atlas| {
            materials.add(StandardMaterial {
                base_color_texture: Some(texture_handles.images.add(atlas)),
                perceptual_roughness: 0.78,
                reflectance: 0.26,
                fog_enabled: false,
                ..default()
            })
        });
        Ok(Self {
            shared,
            finishes,
            arms,
            arm_materials,
        })
    }
}

pub(crate) fn load_world_knives(knife_pack: &[u8]) -> Result<[Mesh; FINISH_COUNT], String> {
    let knife = Pack::read(knife_pack, "knife")?;
    let geometry = knife.geometry()?;
    let mut textures = HashMap::new();
    for map in knife
        .manifest
        .materials
        .iter()
        .filter_map(|material| material.base_map)
    {
        if let std::collections::hash_map::Entry::Vacant(entry) = textures.entry(map) {
            entry.insert(knife.decode_texture(map)?);
        }
    }
    let light = Vec3::new(-0.33, 0.62, 0.70).normalize();
    Ok(std::array::from_fn(|finish| {
        let mut positions = Vec::new();
        let mut normals = Vec::new();
        let mut colors = Vec::new();
        for part in knife
            .manifest
            .parts
            .iter()
            .filter(|part| part.group == finish || SHARED_GROUPS.contains(&part.group))
        {
            let geometry = &geometry[part.mesh];
            let material = knife.material(part);
            let texture = material.base_map.map(|map| &textures[&map]);
            for &index in &geometry.indices {
                let index = index as usize;
                let normal = geometry.normals[index];
                let lighting = 0.68 + 0.32 * Vec3::from_array(normal).dot(light).max(0.0);
                let mut color = material.base_color;
                if let Some(texture) = texture {
                    let pixel = sample_color(texture, geometry.uvs[index]);
                    for channel in 0..4 {
                        color[channel] *= pixel[channel];
                    }
                }
                for channel in &mut color[..3] {
                    *channel *= lighting;
                }
                positions.push(geometry.positions[index]);
                normals.push(normal);
                colors.push(color);
            }
        }
        // The Store also uses these expanded triangles for its software preview.
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
    }))
}

fn sample_color(image: &RgbaImage, uv: [f32; 2]) -> [f32; 4] {
    let x = ((uv[0].clamp(0.0, 1.0) * image.width() as f32) as u32).min(image.width() - 1);
    let y = ((uv[1].clamp(0.0, 1.0) * image.height() as f32) as u32).min(image.height() - 1);
    let [r, g, b, a] = image.get_pixel(x, y).0;
    let color: LinearRgba = Color::srgba_u8(r, g, b, a).into();
    color.to_f32_array()
}

struct Pack<'a> {
    manifest: Manifest,
    data: &'a [u8],
}

impl<'a> Pack<'a> {
    fn read(bytes: &'a [u8], kind: &str) -> Result<Self, String> {
        // SVM1: four-byte signature, manifest length, JSON manifest, little-endian payload.
        let header = bytes
            .get(..8)
            .filter(|header| &header[..4] == b"SVM1")
            .ok_or("Invalid viewmodel pack signature")?;
        let manifest_end = 8usize
            .checked_add(u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize)
            .ok_or("Invalid viewmodel manifest length")?;
        let manifest: Manifest = serde_json::from_slice(
            bytes
                .get(8..manifest_end)
                .ok_or("Truncated viewmodel manifest")?,
        )
        .map_err(|error| format!("Invalid viewmodel manifest: {error}"))?;
        if manifest.kind != kind || manifest.parts.is_empty() {
            return Err(format!("Expected a {kind} viewmodel pack with mesh parts"));
        }
        let knife = kind == "knife";
        let mut groups = [false; 5];
        for part in &manifest.parts {
            let group_count = if knife { groups.len() } else { 1 };
            let valid_material = if knife {
                part.material
                    .is_some_and(|id| id < manifest.materials.len())
            } else {
                part.material.is_none()
            };
            if part.group >= group_count || part.mesh >= manifest.meshes.len() || !valid_material {
                return Err(format!("Invalid {kind} viewmodel part"));
            }
            groups[part.group] = true;
        }
        if knife && groups.contains(&false) {
            return Err("Knife viewmodel is missing a finish or shared part".into());
        }
        for material in &manifest.materials {
            if [
                material.base_map,
                material.orm_map,
                material.normal_map,
                material.occlusion_map,
            ]
            .into_iter()
            .flatten()
            .any(|index| index >= manifest.textures.len())
            {
                return Err("Invalid viewmodel texture reference".into());
            }
        }
        let pack = Self {
            manifest,
            data: &bytes[manifest_end..],
        };
        for &block in &pack.manifest.textures {
            pack.block(block)?;
        }
        Ok(pack)
    }

    fn material(&self, part: &Part) -> &Pbr {
        &self.manifest.materials[part.material.expect("validated knife material")]
    }

    fn block(&self, block: Block) -> Result<&'a [u8], String> {
        block
            .offset
            .checked_add(block.length)
            .and_then(|end| self.data.get(block.offset..end))
            .ok_or_else(|| "Truncated viewmodel payload".into())
    }

    fn geometry(&self) -> Result<Vec<Geometry>, String> {
        self.manifest
            .meshes
            .iter()
            .map(|mesh| Geometry::read(self.block(mesh.vertices)?, self.block(mesh.indices)?))
            .collect()
    }

    fn decode_texture(&self, index: usize) -> Result<RgbaImage, String> {
        let image = image::load_from_memory(self.block(self.manifest.textures[index])?)
            .map_err(|error| format!("Invalid viewmodel texture: {error}"))?;
        if image.width() == 0 || image.height() == 0 {
            return Err("Viewmodel texture has no pixels".into());
        }
        Ok(image.into_rgba8())
    }
}

#[derive(Deserialize)]
struct Manifest {
    kind: String,
    parts: Vec<Part>,
    meshes: Vec<GeometryBlocks>,
    materials: Vec<Pbr>,
    textures: Vec<Block>,
}

#[derive(Deserialize)]
struct Part {
    group: usize,
    mesh: usize,
    material: Option<usize>,
}

#[derive(Deserialize)]
struct GeometryBlocks {
    vertices: Block,
    indices: Block,
}

#[derive(Clone, Copy, Deserialize)]
struct Block {
    offset: usize,
    length: usize,
}

struct Geometry {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl Geometry {
    fn read(vertices: &[u8], indices: &[u8]) -> Result<Self, String> {
        if vertices.is_empty()
            || vertices.len() % VERTEX_BYTES != 0
            || indices.is_empty()
            || indices.len() % 12 != 0
        {
            return Err("Invalid viewmodel mesh buffer length".into());
        }
        let count = vertices.len() / VERTEX_BYTES;
        let mut positions = Vec::with_capacity(count);
        let mut normals = Vec::with_capacity(count);
        let mut uvs = Vec::with_capacity(count);
        for vertex in vertices.chunks_exact(VERTEX_BYTES) {
            let values: [f32; 8] = std::array::from_fn(|i| {
                f32::from_le_bytes(vertex[i * 4..(i + 1) * 4].try_into().unwrap())
            });
            if values.iter().any(|value| !value.is_finite()) {
                return Err("Viewmodel mesh has non-finite vertices".into());
            }
            let [x, y, z, nx, ny, nz, u, v] = values;
            positions.push([x, y, z]);
            normals.push([nx, ny, nz]);
            uvs.push([u, v]);
        }
        let indices: Vec<_> = indices
            .chunks_exact(4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        if indices.iter().any(|&index| index as usize >= count) {
            return Err("Invalid viewmodel triangle index".into());
        }
        Ok(Self {
            positions,
            normals,
            uvs,
            indices,
        })
    }

    fn into_mesh(self, tangents: bool) -> Result<Mesh, String> {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
        .with_inserted_indices(Indices::U32(self.indices));
        if tangents {
            mesh.generate_tangents()
                .map_err(|error| format!("Viewmodel normal map needs valid tangents: {error}"))?;
        }
        Ok(mesh)
    }
}

#[derive(Deserialize)]
struct Pbr {
    base_color: [f32; 4],
    roughness: f32,
    metallic: f32,
    base_map: Option<usize>,
    orm_map: Option<usize>,
    normal_map: Option<usize>,
    normal_scale: f32,
    occlusion_map: Option<usize>,
    double_sided: bool,
}

impl Pbr {
    fn to_material(&self, textures: &mut TextureHandles<'_>) -> StandardMaterial {
        let [r, g, b, a] = self.base_color;
        StandardMaterial {
            base_color: Color::linear_rgba(r, g, b, a),
            base_color_texture: textures.get(self.base_map, true, 1.0),
            perceptual_roughness: self.roughness,
            metallic: self.metallic,
            metallic_roughness_texture: textures.get(self.orm_map, false, 1.0),
            normal_map_texture: textures.get(self.normal_map, false, self.normal_scale),
            occlusion_texture: textures.get(self.occlusion_map, false, 1.0),
            double_sided: self.double_sided,
            cull_mode: if self.double_sided {
                None
            } else {
                Some(Face::Back)
            },
            fog_enabled: false,
            ..default()
        }
    }
}

struct TextureHandles<'a> {
    pixels: &'a [RgbaImage],
    cache: HashMap<(usize, bool, u32), Handle<Image>>,
    images: &'a mut Assets<Image>,
}

impl TextureHandles<'_> {
    fn get(
        &mut self,
        texture: Option<usize>,
        srgb: bool,
        normal_scale: f32,
    ) -> Option<Handle<Image>> {
        let index = texture?;
        let key = (index, srgb, normal_scale.to_bits());
        let handle = self.cache.entry(key).or_insert_with(|| {
            let mut rgba = self.pixels[index].clone();
            if normal_scale != 1.0 {
                for pixel in rgba.pixels_mut() {
                    let [r, g, b, _] = pixel.0;
                    let normal = Vec3::new(
                        (f32::from(r) / 127.5 - 1.0) * normal_scale,
                        (f32::from(g) / 127.5 - 1.0) * normal_scale,
                        f32::from(b) / 127.5 - 1.0,
                    )
                    .try_normalize()
                    .unwrap_or(Vec3::Z);
                    for (channel, value) in pixel.0.iter_mut().zip(normal.to_array()) {
                        *channel = ((value + 1.0) * 127.5).round() as u8;
                    }
                }
            }
            let size = Extent3d {
                width: rgba.width(),
                height: rgba.height(),
                depth_or_array_layers: 1,
            };
            let format = if srgb {
                TextureFormat::Rgba8UnormSrgb
            } else {
                TextureFormat::Rgba8Unorm
            };
            let mut image = Image::new_uninit(
                size,
                TextureDimension::D2,
                format,
                RenderAssetUsages::RENDER_WORLD,
            );
            let mut data = Vec::with_capacity(rgba.as_raw().len() + rgba.as_raw().len() / 3);
            loop {
                data.extend_from_slice(rgba.as_raw());
                let (width, height) = rgba.dimensions();
                if (width, height) == (1, 1) {
                    break;
                }
                rgba = resize(
                    &rgba,
                    (width / 2).max(1),
                    (height / 2).max(1),
                    FilterType::Triangle,
                );
                image.texture_descriptor.mip_level_count += 1;
            }
            image.data = Some(data);
            image.sampler = ImageSampler::linear();
            self.images.add(image)
        });
        Some(handle.clone())
    }
}
