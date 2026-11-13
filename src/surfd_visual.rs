use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    io::Cursor,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use crate::bc_texture::{BcFormat, BcTexture};
use bevy_math::{Mat4, Quat, Vec3};
use image::{ImageReader, Rgba, RgbaImage};
use serde::Deserialize;

use crate::course::{
    Course, CourseFog, CourseTexture, TextureAlpha, VisualInstance, VisualLod, VisualMaterial,
    VisualMesh, VisualOccluder, VisualPropFade, VisualVertex,
};
use crate::visual_opacity::{AreaWindowData, Proximity, VisualOpacity};
use crate::visual_visibility::VisualVisibility;

use super::{Source, VisualLoadOptions, read_asset_exact, read_json, source_axes};

const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_PACK_BYTES: usize = 64 * 1024 * 1024;
const MAX_RESOURCE_BYTES: usize = 512 * 1024 * 1024;
const PARALLEL_PACK_LIMIT: usize = 4 * 1024 * 1024;
static PARALLEL_VISUAL_LOAD: Mutex<()> = Mutex::new(());
const VERTEX_BYTES: usize = 12 * size_of::<f32>();

pub fn visual_dependencies(path: &Path) -> Result<Vec<String>, String> {
    visual_dependencies_for_gpu(path, false)
}

pub fn visual_dependencies_for_gpu(path: &Path, use_bc: bool) -> Result<Vec<String>, String> {
    let manifest = Manifest::read(path)?;
    let plan = manifest.asset_plan(use_bc)?;
    Ok(manifest
        .packs_for(&plan.urls)
        .into_iter()
        .map(|pack| pack.url.clone())
        .collect())
}

pub(super) fn populate(
    course: &mut Course,
    path: &Path,
    map_id: &str,
    source: Option<&Source>,
    options: VisualLoadOptions,
    prepare_packs: impl FnOnce(&[&str]) -> Result<(), String>,
) -> Result<(), String> {
    let mut manifest = Manifest::read(path)?;
    let source_hash = source.and_then(|source| source.sha1.as_deref());
    if manifest.data.map != map_id
        || source_hash.is_some_and(|hash| manifest.data.bsp_sha1.as_deref() != Some(hash))
    {
        return Err(format!(
            "Visual manifest does not match the collision for {map_id}"
        ));
    }
    course.visual_visibility = manifest
        .data
        .visibility
        .take()
        .and_then(|data| VisualVisibility::parse(data, manifest.data.bsp_sha1.as_deref()?));
    let plan = manifest.asset_plan(options.use_bc)?;
    let packs = manifest.packs_for(&plan.urls);
    let urls: Vec<_> = packs.iter().map(|pack| pack.url.as_str()).collect();
    prepare_packs(&urls)?;
    let assets = Manifest::read_packs(path, &packs, &plan.urls, options.parallel_packs)?;
    let decoded_images = prepare_images(&assets, plan.images)?;
    let mut textures = TextureBuilder {
        assets,
        decoded_images,
        variants: plan.variants,
        output: &mut course.textures,
        indices: HashMap::new(),
    };

    for mesh in &manifest.data.world {
        if let Some(mesh) = manifest.data.build_mesh(mesh, &mut textures, &[], 0, 0)? {
            course.visual_meshes.push(mesh);
        }
    }
    for mesh in &manifest.data.sky_occluders {
        let (vertices, indices) = mesh.decode(textures.bytes(&mesh.url)?)?;
        course.sky_occluders.push(VisualOccluder {
            positions: vertices.into_iter().map(|vertex| vertex.position).collect(),
            indices,
        });
    }
    let mut instances = vec![Vec::new(); manifest.data.models.len()];
    for (id, prop) in manifest.data.props.iter().enumerate() {
        if let Some(model_instances) = instances.get_mut(prop.model) {
            model_instances.push((id, prop));
        }
    }
    for (model, props) in manifest.data.models.iter().zip(instances) {
        if props.is_empty() {
            continue;
        }
        let switch_points: Arc<[f32]> = if manifest.data.supports_lods(model) {
            model
                .lods
                .iter()
                .map(|lod| lod.switch_point)
                .take_while(|point| *point >= 0.0)
                .collect()
        } else {
            Arc::default()
        };
        let uses_cubemap = model.meshes.iter().any(|mesh| {
            manifest
                .data
                .mesh_material(mesh)
                .is_some_and(|material| material.parameter_is("$envmap", "env_cubemap"))
        });
        let mut sky_fades = HashMap::<_, Vec<_>>::new();
        for &(_, prop) in &props {
            if prop.sky
                && let Some([start, _]) = prop.fade_range()
            {
                sky_fades
                    .entry(prop.fade_group(uses_cubemap))
                    .or_default()
                    .push((source_axes(Vec3::from_array(prop.origin)), start));
            }
        }
        let sky_fades: HashMap<_, Arc<[(Vec3, f32)]>> = sky_fades
            .into_iter()
            .map(|(key, props)| (key, Arc::from(props)))
            .collect();
        let faded_clusters = if course.visual_visibility.is_some() {
            faded_prop_visibility(props.iter().map(|&(_, prop)| prop), uses_cubemap)
        } else {
            HashMap::new()
        };
        let props: Vec<_> = props
            .into_iter()
            .map(|(id, prop)| PropDraw {
                id,
                prop,
                clusters: if prop.sky {
                    None
                } else {
                    prop.fade
                        .and_then(|_| faded_clusters.get(&prop.fade_group(uses_cubemap)))
                        .cloned()
                        .unwrap_or_else(|| visibility_clusters(prop.visibility_clusters.as_ref()))
                },
                lod: (!switch_points.is_empty() && prop.has_lods(model))
                    .then(|| switch_points.clone()),
                fade: prop.fade_range().map(|range| VisualPropFade {
                    range,
                    sky_group: prop
                        .sky
                        .then(|| sky_fades.get(&prop.fade_group(uses_cubemap)).cloned())
                        .flatten(),
                }),
            })
            .collect();
        let lod_props: Vec<_> = props
            .iter()
            .filter(|prop| prop.lod.is_some())
            .cloned()
            .collect();
        let levels = std::iter::once(model.meshes.as_slice()).chain(
            model
                .lods
                .iter()
                .take(if lod_props.is_empty() {
                    0
                } else {
                    switch_points.len()
                })
                .map(|lod| lod.meshes.as_slice()),
        );
        for (level, meshes) in levels.enumerate() {
            let instances = if level == 0 { &props } else { &lod_props };
            for (part, mesh) in meshes.iter().enumerate() {
                if let Some(mesh) =
                    manifest
                        .data
                        .build_mesh(mesh, &mut textures, instances, part, level)?
                {
                    course.visual_meshes.push(mesh);
                }
            }
        }
    }
    if course.visual_meshes.is_empty() {
        return Err(format!(
            "Visual manifest for {map_id} has no visible meshes"
        ));
    }

    course.sky = textures.sky(&manifest.data.sky, &manifest.data.sky_transforms)?;
    course.fog = Some(manifest.data.fog.as_ref().map_or_else(
        || CourseFog {
            color: [184.0 / 255.0, 203.0 / 255.0, 218.0 / 255.0],
            start: 0.0,
            end: 100_000.0,
            max_density: 0.0,
        },
        Fog::convert,
    ));
    Ok(())
}

#[derive(Deserialize)]
struct Manifest {
    format: u32,
    packs: Vec<Pack>,
    data: VisualData,
}

struct AssetPlan<'a> {
    urls: HashSet<&'a str>,
    images: HashSet<&'a str>,
    variants: HashMap<&'a str, &'a DdsVariant>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DdsVariant {
    url: String,
    byte_length: usize,
    width: u32,
    height: u32,
    mipmaps: u32,
    format: String,
}

impl DdsVariant {
    fn format(&self) -> Option<BcFormat> {
        if self.format == "DXT1A" {
            Some(BcFormat::Bc1)
        } else {
            BcFormat::from_fourcc(self.format.as_bytes())
        }
    }

    fn read(&self, bytes: Vec<u8>) -> Result<BcTexture, String> {
        let size = bytes.len();
        let mut texture =
            BcTexture::read(bytes).map_err(|error| format!("{}: {error}", self.url))?;
        if Some(texture.format) != self.format()
            || texture.width != self.width
            || texture.height != self.height
            || texture.mip_levels != self.mipmaps
            || size != self.byte_length
        {
            return Err(format!(
                "{}: DDS header differs from the visual manifest",
                self.url
            ));
        }
        texture.rgb_only = self.format == "DXT1";
        Ok(texture)
    }
}

impl Manifest {
    fn read(path: &Path) -> Result<Self, String> {
        let result: Self = read_json(path, MAX_MANIFEST_BYTES)?;
        if result.format != 1 || result.data.format != 1 || result.packs.len() > 1024 {
            return Err(format!(
                "Unsupported Surfd visual manifest: {}",
                path.display()
            ));
        }
        for pack in &result.packs {
            pack.file_name()?;
            if pack.byte_length == 0 || pack.byte_length > MAX_PACK_BYTES {
                return Err(format!("Invalid size for visual pack {}", pack.url));
            }
            if !matches!(pack.variant.as_deref(), None | Some("decoded" | "s3tc")) {
                return Err(format!("Unknown visual pack variant: {}", pack.url));
            }
        }
        Ok(result)
    }

    fn packs_for(&self, assets: &HashSet<&str>) -> Vec<&Pack> {
        self.packs
            .iter()
            .filter(|pack| {
                pack.entries.iter().any(|entry| {
                    assets.contains(entry.url.as_str()) && pack.includes(entry.url.as_str())
                })
            })
            .collect()
    }

    fn asset_plan(&self, use_bc: bool) -> Result<AssetPlan<'_>, String> {
        let mut plan = self.data.needed_assets()?;
        if let Some(compressed) = self.data.compressed_textures.as_ref().filter(|_| use_bc) {
            let available: HashSet<&str> = self
                .packs
                .iter()
                .filter(|pack| pack.variant.as_deref() == Some("s3tc"))
                .flat_map(|pack| pack.entries.iter().map(|entry| entry.url.as_str()))
                .collect();
            let decoded: HashSet<&str> = self
                .data
                .sky
                .iter()
                .flatten()
                .map(String::as_str)
                .chain(
                    self.data
                        .cubemaps
                        .iter()
                        .flat_map(|cube| cube.urls.iter().map(String::as_str)),
                )
                .chain(self.data.lightmaps.iter().map(|page| page.url.as_str()))
                .chain(self.data.prop_lighting.iter().map(|page| page.url.as_str()))
                .collect();
            for original in self
                .data
                .materials
                .iter()
                .flat_map(|material| material.texture_urls())
            {
                if let Some(variant) = compressed.get(original)
                    && !decoded.contains(original)
                    && variant.format().is_some()
                    && variant.url.starts_with("/maps/")
                    && variant.url.ends_with(".dds")
                    && available.contains(variant.url.as_str())
                    && [variant.width, variant.height]
                        .into_iter()
                        .all(|size| (4..=16_384).contains(&size) && size % 4 == 0)
                    && plan.urls.remove(original)
                {
                    plan.urls.insert(variant.url.as_str());
                    plan.images.remove(original);
                    plan.variants.insert(original, variant);
                }
            }
        }
        Ok(plan)
    }

    fn read_packs<'a>(
        path: &Path,
        packs: &[&'a Pack],
        needed: &HashSet<&'a str>,
        parallel: bool,
    ) -> Result<HashMap<&'a str, Vec<u8>>, String> {
        let folder = path
            .parent()
            .ok_or("Visual manifest has no asset directory")?;
        let mut buffers: HashMap<&str, (usize, Vec<u8>)> = HashMap::new();
        let mut total = 0_usize;
        let workers = thread::available_parallelism()
            .map_or(1, |count| count.get().saturating_sub(1).clamp(1, 3));
        // Reserve a logical CPU and bound decompressed batches to 12 MiB. Pack
        // inflation shares its worker permit with the image decoder.
        let permit = if parallel
            && workers > 1
            && packs.len() >= 4
            && packs
                .iter()
                .all(|pack| pack.byte_length <= PARALLEL_PACK_LIMIT)
        {
            PARALLEL_VISUAL_LOAD.try_lock().ok()
        } else {
            None
        };
        let mut append = |pack: &'a Pack, bytes: Vec<u8>| {
            for entry in pack
                .entries
                .iter()
                .filter(|entry| needed.contains(entry.url.as_str()) && pack.includes(&entry.url))
            {
                let chunk = entry.slice(&bytes)?;
                if !buffers.contains_key(entry.url.as_str()) {
                    total += entry.asset_length;
                    if total > MAX_RESOURCE_BYTES {
                        return Err("Surfd visual resources exceed the memory limit".into());
                    }
                }
                let (expected, buffer) = buffers.entry(entry.url.as_str()).or_insert_with(|| {
                    (entry.asset_length, Vec::with_capacity(entry.asset_length))
                });
                if *expected != entry.asset_length || buffer.len() != entry.asset_offset {
                    return Err(format!("Invalid visual pack part for {}", entry.url));
                }
                buffer.extend_from_slice(chunk);
            }
            Ok::<(), String>(())
        };
        if permit.is_some() {
            for batch in packs.chunks(workers) {
                // Split entries must be assembled in manifest order, even
                // when later packs finish inflating first.
                for (&pack, bytes) in batch.iter().zip(read_pack_batch(folder, batch)) {
                    append(pack, bytes?)?;
                }
            }
        } else {
            for &pack in packs {
                append(pack, pack.read(folder)?)?;
            }
        }
        needed
            .iter()
            .copied()
            .map(|url| {
                let (_, buffer) = buffers
                    .remove(url)
                    .filter(|(size, data)| *size == data.len())
                    .ok_or_else(|| format!("Missing or incomplete visual asset {url}"))?;
                Ok((url, buffer))
            })
            .collect()
    }
}

fn read_pack_batch(folder: &Path, batch: &[&Pack]) -> Vec<Result<Vec<u8>, String>> {
    let (&first, rest) = batch.split_first().expect("nonempty pack batch");
    thread::scope(|scope| {
        let workers: Vec<_> = rest
            .iter()
            .map(|&pack| {
                thread::Builder::new()
                    .name("surfd-pack-inflate".into())
                    .spawn_scoped(scope, move || pack.read(folder))
                    .map_err(|_| pack)
            })
            .collect();
        std::iter::once(first.read(folder))
            .chain(workers.into_iter().map(|worker| {
                match worker {
                    Ok(worker) => worker
                        .join()
                        .map_err(|_| "Visual pack worker stopped unexpectedly".to_owned())?,
                    Err(pack) => pack.read(folder),
                }
            }))
            .collect()
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pack {
    url: String,
    variant: Option<String>,
    byte_length: usize,
    entries: Vec<Entry>,
}

impl Pack {
    fn read(&self, folder: &Path) -> Result<Vec<u8>, String> {
        read_asset_exact(&folder.join(self.file_name()?), self.byte_length)
    }

    fn includes(&self, url: &str) -> bool {
        (self.variant.as_deref() == Some("s3tc")) == url.ends_with(".dds")
    }

    fn file_name(&self) -> Result<&str, String> {
        let Some(name) = self.url.strip_prefix("/packed/") else {
            return Err(format!("Invalid visual pack URL {}", self.url));
        };
        let valid = name.strip_suffix(".visual-assets.gz").is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        if !valid {
            return Err(format!("Invalid visual pack URL {}", self.url));
        }
        Ok(name)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    url: String,
    offset: usize,
    length: usize,
    asset_offset: usize,
    asset_length: usize,
}

impl Entry {
    fn slice<'a>(&self, bytes: &'a [u8]) -> Result<&'a [u8], String> {
        if self.length == 0
            || self.asset_length > MAX_PACK_BYTES
            || self.asset_offset.saturating_add(self.length) > self.asset_length
        {
            return Err(format!("Invalid visual asset length for {}", self.url));
        }
        bytes
            .get(self.offset..self.offset.saturating_add(self.length))
            .ok_or_else(|| format!("Invalid visual asset range for {}", self.url))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VisualData {
    format: u32,
    map: String,
    bsp_sha1: Option<String>,
    world: Vec<Mesh>,
    #[serde(default)]
    sky_occluders: Vec<Mesh>,
    materials: Vec<Material>,
    compressed_textures: Option<HashMap<String, DdsVariant>>,
    #[serde(default)]
    models: Vec<Model>,
    #[serde(default)]
    props: Vec<Prop>,
    #[serde(default)]
    lightmaps: Vec<Lightmap>,
    #[serde(default)]
    cubemaps: Vec<Cubemap>,
    prop_lighting: Option<Lightmap>,
    prop_lod_lighting: Option<Lightmap>,
    sky_camera: Option<SkyCamera>,
    lighting_response: Option<LightingResponse>,
    #[serde(default)]
    sky: Vec<Option<String>>,
    #[serde(default)]
    sky_transforms: Vec<Option<String>>,
    fog: Option<Fog>,
    visibility: Option<serde_json::Value>,
}

impl VisualData {
    fn needed_assets(&self) -> Result<AssetPlan<'_>, String> {
        let mut models = HashMap::new();
        for prop in &self.props {
            if let Some(model) = self.models.get(prop.model) {
                let lod = prop.has_lods(model) && self.supports_lods(model);
                *models.entry(prop.model).or_insert(false) |= lod;
            }
        }
        let meshes = self.world.iter().chain(
            models
                .iter()
                .flat_map(|(&index, &lod)| self.models[index].draw_meshes(lod)),
        );
        let mut urls = HashSet::new();
        let mut images = HashSet::new();
        for mesh in meshes {
            let (material, lightmap) = self.material_for(mesh)?;
            if !material.visible() {
                continue;
            }
            urls.insert(mesh.url.as_str());
            images.extend(material.texture_urls());
            images.extend(lightmap.map(|page| page.url.as_str()));
        }
        for prop in &self.props {
            if let Some(model) = self.models.get(prop.model)
                && model
                    .draw_meshes(prop.has_lods(model) && self.supports_lods(model))
                    .any(|mesh| {
                        self.mesh_material(mesh)
                            .is_some_and(Material::uses_local_cubemap)
                    })
                && let Some(cube) = self.prop_cubemap(prop)
            {
                images.extend(cube.iter().map(String::as_str));
            }
        }
        if self
            .props
            .iter()
            .any(|prop| prop.lighting.iter().flatten().any(|offset| *offset >= 0))
        {
            images.extend(self.prop_lighting.as_ref().map(|page| page.url.as_str()));
        }
        if models.values().any(|&lod| lod) {
            images.extend(
                self.prop_lod_lighting
                    .as_ref()
                    .map(|page| page.url.as_str()),
            );
        }
        images.extend(self.sky.iter().flatten().map(String::as_str));
        urls.extend(self.sky_occluders.iter().map(|mesh| mesh.url.as_str()));
        urls.extend(images.iter().copied());
        Ok(AssetPlan {
            urls,
            images,
            variants: HashMap::new(),
        })
    }

    fn mesh_material(&self, mesh: &Mesh) -> Option<&Material> {
        self.materials.get(usize::try_from(mesh.material).ok()?)
    }

    fn supports_lods(&self, model: &Model) -> bool {
        self.prop_lod_lighting.is_some()
            && model
                .lods
                .first()
                .is_some_and(|lod| lod.switch_point >= 0.0)
            && model.meshes.iter().all(|mesh| {
                self.mesh_material(mesh)
                    .is_some_and(|material| !material.translucent)
            })
    }

    fn material_for(&self, mesh: &Mesh) -> Result<(&Material, Option<&Lightmap>), String> {
        let material = self
            .mesh_material(mesh)
            .ok_or_else(|| format!("Unknown material for {}", mesh.url))?;
        let page = if mesh.lightmap >= 0 && !material.unlit {
            Some(
                self.lightmaps
                    .get(mesh.lightmap as usize)
                    .ok_or_else(|| format!("Unknown lightmap for {}", mesh.url))?,
            )
        } else {
            None
        };
        Ok((material, page))
    }

    fn prop_cubemap(&self, prop: &Prop) -> Option<&[String; 6]> {
        Some(&self.cubemaps.get(usize::try_from(prop.envmap?).ok()?)?.urls)
    }

    fn build_mesh(
        &self,
        mesh: &Mesh,
        textures: &mut TextureBuilder<'_>,
        props: &[PropDraw<'_>],
        part: usize,
        level: usize,
    ) -> Result<Option<VisualMesh>, String> {
        let (material, page) = self.material_for(mesh)?;
        if !material.visible() || mesh.vertex_count == 0 || mesh.index_count == 0 {
            return Ok(None);
        }
        let buffer = textures.bytes(&mesh.url)?;
        let (vertices, indices) = mesh.decode(buffer)?;
        let prop_light = if level == 0 {
            &self.prop_lighting
        } else {
            &self.prop_lod_lighting
        }
        .as_ref()
        .filter(|_| {
            !material.unlit
                && props
                    .iter()
                    .any(|draw| draw.prop.light_offset(level, part).is_some())
        });
        let instances = props
            .iter()
            .map(|draw| {
                let prop = draw.prop;
                let local_cube = material
                    .uses_local_cubemap()
                    .then(|| self.prop_cubemap(prop))
                    .flatten();
                Ok(VisualInstance {
                    transform: prop.transform()?,
                    light_offset: prop_light.and_then(|_| prop.light_offset(level, part)),
                    environment: local_cube
                        .map(|urls| textures.environment(urls))
                        .transpose()?,
                    lod: draw.lod.as_ref().map(|switch_points| VisualLod {
                        prop: draw.id,
                        level,
                        switch_points: switch_points.clone(),
                        depth_scale: if prop.sky {
                            self.sky_camera
                                .as_ref()
                                .map_or(1.0, |camera| camera.scale.max(1.0))
                        } else {
                            1.0
                        },
                    }),
                    clusters: draw.clusters.clone(),
                    sky: prop.sky,
                    fade: draw.fade.clone(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let lighting = self.lighting_response.as_ref().filter(|_| !mesh.sky);
        let sky_camera = mesh.sky.then(|| {
            self.sky_camera
                .as_ref()
                .map_or((Vec3::ZERO, 1.0), |camera| {
                    (Vec3::from_array(camera.origin), camera.scale.max(1.0))
                })
        });
        Ok(Some(VisualMesh {
            vertices,
            indices,
            material: VisualMaterial {
                base: textures.texture(material.base.as_deref())?,
                second: textures.texture(material.base2.as_deref())?,
                detail: textures.texture(material.detail.as_deref())?,
                normal: textures.texture(material.normal.as_deref())?,
                env_mask: textures.texture(material.env_mask.as_deref())?,
                environment: material
                    .envmap
                    .as_ref()
                    .map(|urls| textures.environment(urls))
                    .transpose()?,
                lightmap: textures.texture(page.map(|page| page.url.as_str()))?,
                rgb_exp_lightmap: page
                    .is_some_and(|page| page.encoding.as_deref() == Some("rgbexp32-offset128")),
                prop_light: textures.texture(prop_light.map(|page| page.url.as_str()))?,
                tint: material.color,
                opacity: material.opacity(),
                alpha_cut: (material.alpha_test > 0.0
                    || material.parameter_is("$vertexalpha", "1"))
                .then_some(material.alpha_test),
                vertex_alpha: material.parameter_is("$vertexalpha", "1"),
                translucent: material.translucent,
                double_sided: material.no_cull,
                additive: material.additive,
                no_fog: material.no_fog,
                unlit: material.unlit,
                detail_scale: material.detail_scale.values(),
                detail_factor: material.detail_factor,
                seamless: if props.is_empty() {
                    material.seamless
                } else {
                    0.0
                },
                sky_camera,
                env_tint: material.env_tint,
                env_shaping: material.env_shaping,
                normal_alpha_mask: material.normal_alpha_env_mask,
                apply_normal: material.normal.is_some() && !material.parameter_is("$ssbump", "1"),
                reflection_only: material.reflection_only,
                lighting_response: lighting.map(|value| [value.exposure, value.shadow_lift]),
                preserve_shadow_color: lighting.is_some_and(|value| value.preserve_shadow_color),
            },
            instances,
            clusters: (!mesh.sky && mesh.area_window.is_none() && mesh.proximity_alpha.is_none())
                .then(|| visibility_clusters(mesh.visibility_clusters.as_ref()))
                .flatten(),
            sky: mesh.sky,
            opacity: mesh
                .proximity_alpha
                .as_ref()
                .filter(|curve| curve.valid())
                .cloned()
                .map(VisualOpacity::Proximity)
                .or_else(|| {
                    mesh.area_window
                        .as_ref()
                        .map(|window| VisualOpacity::AreaWindow(window.into()))
                }),
        }))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Mesh {
    url: String,
    vertex_count: usize,
    index_count: usize,
    material: i32,
    #[serde(default = "missing_lightmap")]
    lightmap: i32,
    #[serde(default)]
    sky: bool,
    visibility_clusters: Option<serde_json::Value>,
    area_window: Option<AreaWindowData>,
    proximity_alpha: Option<Proximity>,
}

fn visibility_clusters(value: Option<&serde_json::Value>) -> Option<Arc<[usize]>> {
    let values = value?.as_array()?;
    if values.is_empty() {
        return None;
    }
    values
        .iter()
        .map(|value| usize::try_from(value.as_u64()?).ok())
        .collect()
}

fn faded_prop_visibility<'a>(
    props: impl IntoIterator<Item = &'a Prop>,
    uses_cubemap: bool,
) -> HashMap<PropFadeGroup, Option<Arc<[usize]>>> {
    // The reference submits a whole faded prop group when any member passes PVS.
    let mut groups = HashMap::<_, Option<Vec<_>>>::new();
    for prop in props
        .into_iter()
        .filter(|prop| !prop.sky && prop.fade.is_some())
    {
        let group = groups
            .entry(prop.fade_group(uses_cubemap))
            .or_insert_with(|| Some(Vec::new()));
        if let Some(merged) = group {
            match visibility_clusters(prop.visibility_clusters.as_ref()) {
                Some(clusters) => merged.extend_from_slice(&clusters),
                None => *group = None,
            }
        }
    }
    groups
        .into_iter()
        .map(|(key, clusters)| {
            let clusters = clusters.map(|mut clusters| {
                clusters.sort_unstable();
                clusters.dedup();
                Arc::from(clusters)
            });
            (key, clusters)
        })
        .collect()
}

fn missing_lightmap() -> i32 {
    -1
}

impl Mesh {
    fn decode(&self, bytes: &[u8]) -> Result<(Vec<VisualVertex>, Vec<u32>), String> {
        let vertex_end = self
            .vertex_count
            .checked_mul(VERTEX_BYTES)
            .ok_or_else(|| format!("Invalid vertex count in {}", self.url))?;
        if !self.index_count.is_multiple_of(3)
            || vertex_end.checked_add(self.index_count.saturating_mul(4)) != Some(bytes.len())
        {
            return Err(format!("Invalid mesh size in {}", self.url));
        }
        let (vertices, indices) = bytes.split_at(vertex_end);
        let (records, _) = vertices.as_chunks::<VERTEX_BYTES>();
        let vertices = records
            .iter()
            .map(|record| {
                let (data, _) = record.as_chunks::<4>();
                let mut values = data.iter().copied().map(f32::from_le_bytes);
                let position = source_axes(Vec3::from_array(std::array::from_fn(|_| {
                    values.next().unwrap()
                })));
                let normal = source_axes(Vec3::from_array(std::array::from_fn(|_| {
                    values.next().unwrap()
                })));
                let uv = std::array::from_fn(|_| values.next().unwrap());
                let light_uv = std::array::from_fn(|_| values.next().unwrap());
                let blend = values.next().unwrap();
                let light_index = values.next().unwrap();
                if !position.is_finite()
                    || !normal.is_finite()
                    || !uv
                        .into_iter()
                        .chain(light_uv)
                        .chain([blend, light_index])
                        .all(f32::is_finite)
                {
                    return Err(format!("Invalid mesh vertex in {}", self.url));
                }
                Ok(VisualVertex {
                    position,
                    normal,
                    uv,
                    light_uv,
                    blend,
                    light_index,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (indices, _) = indices.as_chunks::<4>();
        let indices: Vec<_> = indices.iter().copied().map(u32::from_le_bytes).collect();
        if indices
            .iter()
            .any(|&index| index as usize >= vertices.len())
        {
            return Err(format!("Invalid mesh index in {}", self.url));
        }
        Ok((vertices, indices))
    }
}

#[derive(Deserialize)]
struct Model {
    meshes: Vec<Mesh>,
    #[serde(default)]
    lods: Vec<ModelLod>,
}

impl Model {
    fn draw_meshes(&self, lods: bool) -> impl Iterator<Item = &Mesh> {
        self.meshes.iter().chain(
            self.lods
                .iter()
                .take_while(move |lod| lods && lod.switch_point >= 0.0)
                .flat_map(|lod| &lod.meshes),
        )
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelLod {
    switch_point: f32,
    meshes: Vec<Mesh>,
}

#[derive(Clone)]
struct PropDraw<'a> {
    id: usize,
    prop: &'a Prop,
    clusters: Option<Arc<[usize]>>,
    lod: Option<Arc<[f32]>>,
    fade: Option<VisualPropFade>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Prop {
    model: usize,
    origin: [f32; 3],
    #[serde(default)]
    angles: [f32; 3],
    #[serde(default = "one")]
    scale: f32,
    #[serde(default)]
    lighting: Vec<Option<i32>>,
    #[serde(default)]
    lod_lighting: Vec<Vec<Option<i32>>>,
    #[serde(default)]
    sky: bool,
    fade: Option<[f32; 2]>,
    visibility_clusters: Option<serde_json::Value>,
    envmap: Option<i32>,
}

impl Prop {
    fn fade_range(&self) -> Option<[f32; 2]> {
        self.fade.filter(|[start, end]| end > start)
    }

    fn fade_group(&self, uses_cubemap: bool) -> PropFadeGroup {
        (
            self.envmap.filter(|_| uses_cubemap),
            self.origin.map(|axis| (axis / 4096.0).floor() as i32),
        )
    }

    fn has_lods(&self, model: &Model) -> bool {
        self.fade.is_none() && self.lod_lighting.len() == model.lods.len()
    }

    fn light_offset(&self, level: usize, part: usize) -> Option<u32> {
        let offsets = if level == 0 {
            &self.lighting
        } else {
            self.lod_lighting.get(level - 1)?
        };
        u32::try_from((*offsets.get(part)?)?).ok()
    }

    fn transform(&self) -> Result<Mat4, String> {
        let [pitch, yaw, roll] = self.angles.map(f32::to_radians);
        let origin = Vec3::from_array(self.origin);
        if !origin.is_finite()
            || !self.scale.is_finite()
            || self.scale <= 0.0
            || ![pitch, yaw, roll].into_iter().all(f32::is_finite)
        {
            return Err("Invalid Surfd prop transform".into());
        }
        let rotation =
            Quat::from_rotation_z(yaw) * Quat::from_rotation_y(pitch) * Quat::from_rotation_x(roll);
        let axes = Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        let authored =
            Mat4::from_scale_rotation_translation(Vec3::splat(self.scale), rotation, origin);
        Ok(axes * authored * axes.transpose())
    }
}

type PropFadeGroup = (Option<i32>, [i32; 3]);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Material {
    name: String,
    base: Option<String>,
    base2: Option<String>,
    normal: Option<String>,
    detail: Option<String>,
    env_mask: Option<String>,
    envmap: Option<[String; 6]>,
    #[serde(default)]
    detail_scale: DetailScale,
    #[serde(default = "one")]
    detail_factor: f32,
    #[serde(default = "white")]
    color: [f32; 3],
    #[serde(default = "one")]
    alpha: f32,
    #[serde(default)]
    translucent: bool,
    #[serde(default)]
    alpha_test: f32,
    #[serde(default)]
    no_cull: bool,
    #[serde(default)]
    additive: bool,
    #[serde(default)]
    no_fog: bool,
    #[serde(default)]
    unlit: bool,
    #[serde(default)]
    seamless: f32,
    shader: Option<String>,
    #[serde(default, deserialize_with = "deserialize_env_tint")]
    env_tint: [f32; 3],
    env_shaping: Option<[f32; 3]>,
    #[serde(default)]
    normal_alpha_env_mask: bool,
    #[serde(default)]
    reflection_only: bool,
    #[serde(default)]
    source_parameters: HashMap<String, serde_json::Value>,
}

impl Material {
    fn visible(&self) -> bool {
        (self.name != "project_tendies/cloods" || self.base.is_some())
            && !matches!(self.shader.as_deref(), Some("nodraw"))
    }

    fn opacity(&self) -> f32 {
        match self.shader.as_deref() {
            Some("refract") => 0.24,
            Some("water") => 0.48,
            _ => self.alpha.clamp(0.0, 1.0),
        }
    }

    fn texture_urls(&self) -> impl Iterator<Item = &str> {
        [
            &self.base,
            &self.base2,
            &self.normal,
            &self.detail,
            &self.env_mask,
        ]
        .into_iter()
        .filter_map(|url| url.as_deref())
        .chain(self.envmap.iter().flatten().map(String::as_str))
    }

    fn parameter_is(&self, key: &str, value: &str) -> bool {
        self.source_parameters
            .get(key)
            .and_then(serde_json::Value::as_str)
            == Some(value)
    }

    fn uses_local_cubemap(&self) -> bool {
        self.envmap.is_none() && self.parameter_is("$envmap", "env_cubemap")
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DetailScale {
    Uniform(f32),
    Uv([f32; 2]),
}

impl Default for DetailScale {
    fn default() -> Self {
        Self::Uniform(1.0)
    }
}

impl DetailScale {
    fn values(&self) -> [f32; 2] {
        match self {
            Self::Uniform(scale) => [*scale; 2],
            Self::Uv(scale) => *scale,
        }
    }
}

#[derive(Deserialize)]
struct Cubemap {
    urls: [String; 6],
}

#[derive(Deserialize)]
struct SkyCamera {
    origin: [f32; 3],
    scale: f32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LightingResponse {
    #[serde(default = "one")]
    exposure: f32,
    #[serde(default)]
    shadow_lift: f32,
    #[serde(default)]
    preserve_shadow_color: bool,
}

#[derive(Deserialize)]
struct Lightmap {
    url: String,
    encoding: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fog {
    #[serde(default)]
    enabled: bool,
    color: [f32; 3],
    start: f32,
    end: f32,
    #[serde(default = "one")]
    max_density: f32,
}

impl Fog {
    fn convert(&self) -> CourseFog {
        CourseFog {
            color: self.color.map(|channel| (channel / 255.0).clamp(0.0, 1.0)),
            start: self.start,
            end: if self.end > self.start {
                self.end
            } else {
                self.start + 1.0
            },
            max_density: if self.enabled {
                self.max_density.clamp(0.0, 1.0)
            } else {
                0.0
            },
        }
    }
}

type DecodedImages<'a> = HashMap<&'a str, Result<RgbaImage, String>>;

fn prepare_images<'a>(
    assets: &HashMap<&'a str, Vec<u8>>,
    images: HashSet<&'a str>,
) -> Result<DecodedImages<'a>, String> {
    let workers = thread::available_parallelism().map_or(1, |count| count.get().min(4));
    if workers == 1 || images.len() < 4 {
        return Ok(HashMap::new());
    }

    // Rapid map changes can leave two import jobs in flight. Let one import use
    // the extra workers while the others follow the ordinary decoder path.
    let Ok(_permit) = PARALLEL_VISUAL_LOAD.try_lock() else {
        return Ok(HashMap::new());
    };
    let mut jobs: Vec<_> = images
        .into_iter()
        .filter_map(|url| assets.get(url).map(|bytes| (url, bytes.as_slice())))
        .collect();
    jobs.sort_unstable_by_key(|(_, bytes)| Reverse(bytes.len()));

    let next = AtomicUsize::new(0);
    let decode_jobs = || {
        let mut decoded = Vec::new();
        while let Some(&(url, bytes)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
            decoded.push((url, decode_image(url, bytes)));
        }
        decoded
    };
    thread::scope(|scope| {
        let mut threads = Vec::with_capacity(workers - 1);
        for _ in 1..workers {
            let Ok(worker) = thread::Builder::new()
                .name("surfd-image-decode".into())
                .spawn_scoped(scope, &decode_jobs)
            else {
                break;
            };
            threads.push(worker);
        }
        let mut decoded: DecodedImages = decode_jobs().into_iter().collect();
        for worker in threads {
            decoded.extend(
                worker
                    .join()
                    .map_err(|_| "Visual image decoder stopped unexpectedly".to_owned())?,
            );
        }
        Ok(decoded)
    })
}

fn decode_image(url: &str, data: &[u8]) -> Result<RgbaImage, String> {
    let mut decoder = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|err| format!("{url}: {err}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    decoder.limits(limits);
    decoder
        .decode()
        .map(|image| image.into_rgba8())
        .map_err(|err| format!("{url}: {err}"))
}

struct TextureBuilder<'a> {
    assets: HashMap<&'a str, Vec<u8>>,
    decoded_images: DecodedImages<'a>,
    variants: HashMap<&'a str, &'a DdsVariant>,
    output: &'a mut Vec<CourseTexture>,
    indices: HashMap<String, usize>,
}

impl TextureBuilder<'_> {
    fn bytes(&self, url: &str) -> Result<&[u8], String> {
        self.assets
            .get(url)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("Missing visual asset {url}"))
    }

    fn image(&mut self, url: &str) -> Result<RgbaImage, String> {
        if let Some(image) = self.decoded_images.remove(url) {
            return image;
        }
        decode_image(url, self.bytes(url)?)
    }

    fn add(&mut self, key: String, name: String, image: RgbaImage, alpha: TextureAlpha) -> usize {
        let index = self.output.len();
        self.output.push(CourseTexture {
            name,
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
            bc: None,
            alpha,
        });
        self.indices.insert(key, index);
        index
    }

    fn texture(&mut self, url: Option<&str>) -> Result<Option<usize>, String> {
        url.map(|url| {
            let variant = self.variants.get(url).copied();
            let key = variant.map_or(url, |variant| variant.url.as_str());
            if let Some(&index) = self.indices.get(key) {
                return Ok(index);
            }
            if let Some(variant) = variant {
                let bytes = self
                    .assets
                    .remove(key)
                    .ok_or_else(|| format!("Missing visual asset {key}"))?;
                let bc = variant.read(bytes)?;
                let index = self.output.len();
                self.output.push(CourseTexture {
                    name: url.into(),
                    width: bc.width,
                    height: bc.height,
                    rgba: Vec::new(),
                    bc: Some(bc),
                    alpha: TextureAlpha::Opaque,
                });
                self.indices.insert(key.into(), index);
                return Ok(index);
            }
            let image = self.image(url)?;
            Ok(self.add(url.into(), url.into(), image, TextureAlpha::Opaque))
        })
        .transpose()
    }

    fn environment(&mut self, urls: &[String; 6]) -> Result<[usize; 6], String> {
        let mut faces = [0; 6];
        // The export orders faces along Source axes. Cube sampling uses the Y-up order.
        for (face, source) in [0, 1, 4, 5, 3, 2].into_iter().enumerate() {
            faces[face] = self.texture(Some(&urls[source]))?.unwrap();
        }
        Ok(faces)
    }

    fn sky(
        &mut self,
        urls: &[Option<String>],
        transforms: &[Option<String>],
    ) -> Result<Option<[usize; 6]>, String> {
        if urls.len() != 6 || urls.iter().any(Option::is_none) {
            return Ok(None);
        }
        let images = urls
            .iter()
            .flatten()
            .map(|url| self.image(url))
            .collect::<Result<Vec<_>, _>>()?;
        let size = images
            .iter()
            .map(|image| image.width().max(image.height()))
            .max()
            .unwrap();
        if size > 4096 {
            return Err("Surfd sky cubemap exceeds the supported face size".into());
        }
        let mut faces = [0; 6];
        // Three.js reflects the world X axis when sampling this cube. Move that
        // reflection into the authored face order and texels for Bevy's cube.
        for (face, authored) in [1, 0, 2, 3, 4, 5].into_iter().enumerate() {
            let image = &images[authored];
            let transform = transforms
                .get(authored)
                .and_then(Option::as_deref)
                .unwrap_or("");
            let rendered = remap_sky(image, size, transform);
            let name = format!("sky face {face}");
            faces[face] = self.add(name.clone(), name, rendered, TextureAlpha::Opaque);
        }
        Ok(Some(faces))
    }
}

fn remap_sky(image: &RgbaImage, size: u32, transform: &str) -> RgbaImage {
    if transform.is_empty() && image.width() == size && image.height() == size {
        return image::imageops::flip_horizontal(image);
    }
    let directive = |name: &str, default: [f32; 2]| {
        let mut values = transform.split_whitespace();
        if values
            .find(|token| token.eq_ignore_ascii_case(name))
            .is_none()
        {
            return default;
        }
        let first = values.next().and_then(|s| s.parse::<f32>().ok());
        let second = values.next().and_then(|s| s.parse::<f32>().ok());
        [
            first.filter(|v| v.is_finite()).unwrap_or(default[0]),
            second.filter(|v| v.is_finite()).unwrap_or(default[1]),
        ]
    };
    let [center_x, center_y] = directive("center", [0.5, 0.5]);
    let [scale_x, scale_y] = directive("scale", [1.0, 1.0]);
    let [translation_x, translation_y] = directive("translate", [0.0, 0.0]);
    let [degrees, _] = directive("rotate", [0.0, 0.0]);
    // Most exported faces spell out the identity transform. On square,
    // power-of-two faces these coordinates land exactly at texel centers.
    if image.width() == size
        && image.height() == size
        && size.is_power_of_two()
        && [center_x, center_y] == [0.5, 0.5]
        && [scale_x, scale_y] == [1.0, 1.0]
        && [translation_x, translation_y] == [0.0, 0.0]
        && degrees == 0.0
    {
        return image::imageops::flip_horizontal(image);
    }
    let linear = std::array::from_fn(|channel| decode_srgb(channel as u8));
    let (sin, cos) = degrees.to_radians().sin_cos();
    RgbaImage::from_fn(size, size, |x, y| {
        let u = 1.0 - (x as f32 + 0.5) / size as f32;
        let v = (y as f32 + 0.5) / size as f32;
        let u_scaled = (u - center_x) * scale_x;
        let v_scaled = (v - center_y) * scale_y;
        let x = cos * u_scaled - sin * v_scaled + center_x + translation_x;
        let y = sin * u_scaled + cos * v_scaled + center_y + translation_y;
        sample_sky(image, x, y, &linear)
    })
}

fn sample_sky(image: &RgbaImage, u: f32, v: f32, linear: &[f32; 256]) -> Rgba<u8> {
    let x = (u * image.width() as f32 - 0.5).clamp(0.0, (image.width() - 1) as f32);
    let y = (v * image.height() as f32 - 0.5).clamp(0.0, (image.height() - 1) as f32);
    let x0 = x as u32;
    let y0 = y as u32;
    let x1 = (x0 + 1).min(image.width() - 1);
    let y1 = (y0 + 1).min(image.height() - 1);
    let (tx, ty) = (x.fract(), y.fract());
    let [a, b, c, d] = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| image.get_pixel(x, y));
    Rgba(std::array::from_fn(|i| {
        let decode = |pixel: &Rgba<u8>| {
            if i == 3 {
                f32::from(pixel[i]) / 255.0
            } else {
                linear[usize::from(pixel[i])]
            }
        };
        let top = decode(a) * (1.0 - tx) + decode(b) * tx;
        let bottom = decode(c) * (1.0 - tx) + decode(d) * tx;
        let value = top * (1.0 - ty) + bottom * ty;
        if i == 3 {
            encode_unorm(value)
        } else {
            encode_srgb(value)
        }
    }))
}

fn one() -> f32 {
    1.0
}

fn white() -> [f32; 3] {
    [1.0; 3]
}

fn deserialize_env_tint<'de, D: serde::Deserializer<'de>>(value: D) -> Result<[f32; 3], D::Error> {
    let components = Vec::<f64>::deserialize(value)?;
    match components.as_slice() {
        [hex] => {
            // The reference Color constructor treats a single channel as packed sRGB.
            let packed = hex.floor().rem_euclid(4_294_967_296.0) as u32;
            Ok([16, 8, 0].map(|shift| decode_srgb((packed >> shift) as u8)))
        }
        [red, green, blue, ..] => Ok([*red as f32, *green as f32, *blue as f32]),
        [red, green] => Ok([*red as f32, *green as f32, 0.0]),
        [] => Ok(white()),
    }
}

fn decode_srgb(channel: u8) -> f32 {
    let value = f32::from(channel) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn encode_srgb(value: f32) -> u8 {
    if value <= 0.0031308 {
        encode_unorm(value * 12.92)
    } else {
        encode_unorm(1.055 * value.powf(1.0 / 2.4) - 0.055)
    }
}

fn encode_unorm(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the original Lt Omnific and Whiteout visual manifests"]
    fn faded_props_use_their_reference_groups_for_pvs() {
        let directory = std::path::PathBuf::from(
            std::env::var_os("SURFD_VISUAL_MANIFESTS").expect("Set SURFD_VISUAL_MANIFESTS"),
        );
        for (map, eye, visible, hidden) in [
            ("lt-omnific", [-3200.0, 0.0, 4160.0], 840, 844),
            ("whiteout", [-7852.0, -14540.0, 15936.0], 806, 32),
        ] {
            let Manifest { mut data, .. } =
                read_json(&directory.join(format!("{map}.gz")), MAX_MANIFEST_BYTES).unwrap();
            let visibility = VisualVisibility::parse(
                data.visibility.take().unwrap(),
                data.bsp_sha1.as_deref().unwrap(),
            )
            .unwrap();
            let eye = Vec3::from_array(eye);
            let clusters = visibility.clusters_near(source_axes(eye)).unwrap();
            let mut mask = Vec::new();
            assert!(visibility.merge_rows(&clusters, &mut mask, &mut Vec::new()));

            let (visible_prop, hidden_prop) = (&data.props[visible], &data.props[hidden]);
            assert_eq!(visible_prop.model, hidden_prop.model);
            let model_id = hidden_prop.model;
            let uses_cubemap = data.models[model_id].meshes.iter().any(|mesh| {
                data.mesh_material(mesh)
                    .is_some_and(|material| material.parameter_is("$envmap", "env_cubemap"))
            });
            let group = hidden_prop.fade_group(uses_cubemap);
            assert_eq!(visible_prop.fade_group(uses_cubemap), group);
            let visible_clusters =
                visibility_clusters(visible_prop.visibility_clusters.as_ref()).unwrap();
            let hidden_clusters =
                visibility_clusters(hidden_prop.visibility_clusters.as_ref()).unwrap();
            assert!(visibility.target_visible(&mask, &visible_clusters), "{map}");
            assert!(!visibility.target_visible(&mask, &hidden_clusters), "{map}");
            assert!(
                eye.distance(Vec3::from_array(hidden_prop.origin))
                    < hidden_prop.fade_range().unwrap()[1]
            );

            let grouped = faded_prop_visibility(
                data.props.iter().filter(|prop| prop.model == model_id),
                uses_cubemap,
            );
            assert!(
                visibility.target_visible(&mask, grouped[&group].as_deref().unwrap()),
                "{map}"
            );

            data.props[visible].visibility_clusters = None;
            let grouped = faded_prop_visibility(
                data.props.iter().filter(|prop| prop.model == model_id),
                uses_cubemap,
            );
            assert!(grouped[&group].is_none(), "{map}");
        }
    }

    #[test]
    fn packed_mesh_keeps_uvs_and_rejects_indices_outside_the_vertex_buffer() {
        let mesh = Mesh {
            url: "fixture.bin".to_owned(),
            vertex_count: 3,
            index_count: 3,
            material: 0,
            lightmap: 0,
            sky: false,
            visibility_clusters: None,
            area_window: None,
            proximity_alpha: None,
        };
        let vertex = [
            2.0_f32, 3.0, 4.0, 0.0, 0.0, 1.0, 0.2, 0.4, 0.6, 0.8, 1.0, 0.0,
        ];
        let mut bytes: Vec<_> = vertex
            .repeat(3)
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        bytes.extend([0_u32, 1, 2].into_iter().flat_map(u32::to_le_bytes));
        let (vertices, indices) = mesh.decode(&bytes).expect("valid packed triangle");
        assert_eq!(vertices[0].position, Vec3::new(2.0, 4.0, -3.0));
        assert_eq!(vertices[0].normal, Vec3::Y);
        assert_eq!(vertices[0].uv, [0.2, 0.4]);
        assert_eq!(vertices[0].light_uv, [0.6, 0.8]);
        assert_eq!(vertices[0].blend, 1.0);
        assert_eq!(indices, [0, 1, 2]);
        bytes[VERTEX_BYTES * 3] = 3;
        assert!(mesh.decode(&bytes).is_err());
    }
}
