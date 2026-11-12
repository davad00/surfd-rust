//! Static Quake III Arena and DeFRaG course import.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    io::Cursor,
    path::Path,
};

use bevy_math::{Vec2, Vec3};
use vformats::{Limits, bsp::ZipReader};

use crate::{
    catalog::MapMode,
    course::{
        Brush, Course, CourseTexture, MapTriggers, Plane, Surface, SurfaceLightmap, SurfaceTexture,
        TextureAlpha, Zone, ZoneKind,
    },
    source_bsp::{Entity, read_bsp_bytes, read_entities, source_axes},
};

const HEADER_BYTES: usize = 8 + 17 * 8;
const LIGHTMAP_EDGE: usize = 128;
const LIGHTMAP_BYTES: usize = LIGHTMAP_EDGE * LIGHTMAP_EDGE * 3;
const PATCH_STEPS: usize = 4;
const MAX_TRIANGLES: usize = 1_000_000;
const MAX_IMAGE_BYTES: usize = 256 * 1024 * 1024;
const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MAP_BYTES: u64 = 256 * 1024 * 1024;
const CONTENTS_SOLID: i32 = 0x1;
const CONTENTS_PLAYERCLIP: i32 = 0x1_0000;
const SURF_SKY: i32 = 0x4;
const SURF_NODRAW: i32 = 0x80;
const SURF_SKIP: i32 = 0x200;

pub(crate) fn is_bsp(bytes: &[u8]) -> bool {
    bytes.starts_with(b"IBSP\x2e\x00\x00\x00")
}

/// Loads a Quake III IBSP 46 map or a PK3 containing one map. Original media
/// packaged with the map is read in memory. Imported runs are local practice.
pub fn load(path: &Path) -> Result<Course, String> {
    load_file(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn load_file(path: &Path) -> Result<Course, String> {
    let data = read_bsp_bytes(path)?;
    let file_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or("Expected a Quake III map filename")?;
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pk3"))
    {
        let pack = ZipReader::parse(&data).map_err(|e| format!("Invalid PK3: {e}"))?;
        let mut assets = Assets::new(Some(&pack), path);
        let mut candidates = assets.entries.keys().filter(|name| {
            name.strip_prefix("maps/")
                .is_some_and(|name| !name.contains('/') && name.ends_with(".bsp"))
        });
        let first = candidates.next().ok_or("PK3 has no BSP under maps/")?;
        let preferred = format!("maps/{}.bsp", file_name.to_ascii_lowercase());
        let bsp = if assets.entries.contains_key(&preferred) {
            &preferred
        } else if candidates.next().is_none() {
            first
        } else {
            return Err("PK3 contains multiple maps and none matches the filename".into());
        };
        let name = bsp
            .trim_start_matches("maps/")
            .trim_end_matches(".bsp")
            .to_owned();
        let data = assets
            .packed_file(bsp, MAX_MAP_BYTES)
            .ok_or("Could not read the BSP from the PK3")?;
        let owned = data.into_owned();
        return decode(name, &owned, &mut assets);
    }
    let name = file_name.strip_suffix(".bsp").unwrap_or(file_name);
    decode(name.into(), &data, &mut Assets::new(None, path))
}

fn decode(name: String, data: &[u8], assets: &mut Assets<'_>) -> Result<Course, String> {
    let bsp = Bsp::read(data)?;
    let entity_text = String::from_utf8_lossy(bsp.entities);
    let entities = read_entities(&entity_text)?;
    let world = bsp
        .models
        .first()
        .ok_or("Quake III BSP has no world model")?;
    let player = entities.iter().find(|entity| {
        matches!(
            classname(entity),
            "info_player_start" | "info_player_deathmatch" | "info_player_intermission"
        ) && entity.origin().is_some()
    });
    let spawn = player
        .and_then(Entity::origin)
        .ok_or("Quake III BSP has no player start")?;
    let mut brushes = Vec::new();
    let mut surfaces = Vec::new();
    let mut zones = Vec::new();
    let lightmaps = assets.lightmaps(bsp.lightmaps)?;
    let mut sky = None;
    let mut world_models = HashSet::new();
    world_models.insert(0);
    bsp.add_model(
        world,
        Vec3::ZERO,
        assets,
        &lightmaps,
        &mut brushes,
        &mut surfaces,
        &mut sky,
    )?;

    let mut targets = HashMap::new();
    let mut checkpoints = HashMap::new();
    for (index, entity) in entities.iter().enumerate() {
        if let Some(name) = entity.get("targetname") {
            targets.entry(name).or_insert(index);
        }
        if classname(entity).eq_ignore_ascii_case("target_checkpoint") {
            let ordinal =
                u16::try_from(checkpoints.len() + 1).map_err(|_| "Too many checkpoints")?;
            checkpoints.insert(index, ordinal);
        }
    }
    for entity in &entities {
        let Some(model_index) = entity.model_index() else {
            continue;
        };
        if model_index == 0 {
            continue;
        }
        let model = bsp
            .models
            .get(model_index)
            .ok_or("Invalid Quake III entity model")?;
        let offset = entity.origin().unwrap_or_default();
        if matches!(classname(entity), "func_static" | "func_group")
            && world_models.insert(model_index)
        {
            bsp.add_model(
                model,
                offset,
                assets,
                &lightmaps,
                &mut brushes,
                &mut surfaces,
                &mut sky,
            )?;
        }
        let target = resolve_target(entity, &entities, &targets);
        let kind = match classname(entity) {
            "trigger_multiple" | "trigger_once" => target.and_then(|index| {
                let class = classname(&entities[index]);
                if class.eq_ignore_ascii_case("target_starttimer") {
                    Some(ZoneKind::Start)
                } else if class.eq_ignore_ascii_case("target_stoptimer") {
                    Some(ZoneKind::Finish)
                } else {
                    checkpoints.get(&index).copied().map(ZoneKind::Checkpoint)
                }
            }),
            "trigger_teleport" => target.and_then(|index| {
                let destination = &entities[index];
                Some(ZoneKind::Teleport {
                    position: destination.origin()?,
                    yaw: Some(destination.yaw()),
                })
            }),
            "trigger_hurt" => {
                let damage = entity
                    .get("dmg")
                    .and_then(|value| value.parse::<u32>().ok());
                damage
                    .filter(|&damage| damage >= 100)
                    .map(|_| ZoneKind::Reset)
            }
            _ => None,
        };
        let Some(kind) = kind else { continue };
        for brush in row_range(&bsp.brushes, model, 32, "model brushes")? {
            zones.push(Zone {
                brush: bsp.brush(brush, offset)?,
                kind: kind.clone(),
            });
        }
    }
    if brushes.is_empty() {
        return Err("Quake III BSP has no solid player geometry".into());
    }
    let kill_y = brushes
        .iter()
        .map(|brush| brush.min.y)
        .fold(spawn.y, f32::min)
        - 512.0;
    Ok(Course {
        name,
        catalog_revision: None,
        bonus_routes: Vec::new(),
        map_triggers: MapTriggers {
            source_mode: Some(MapMode::Bhop),
            source_world_limit: Some(131_072.0),
            ..MapTriggers::default()
        },
        spawn,
        spawn_yaw: player.map(Entity::yaw).unwrap_or_default(),
        brushes,
        surfaces,
        textures: std::mem::take(&mut assets.textures),
        visual_meshes: Vec::new(),
        visual_visibility: None,
        practice_spawns: Vec::new(),
        sky,
        sky_occluders: Vec::new(),
        fog: None,
        zones,
        kill_y,
    })
}

fn classname<'a>(entity: &Entity<'a>) -> &'a str {
    entity.get("classname").unwrap_or_default()
}

fn resolve_target(
    source: &Entity<'_>,
    entities: &[Entity<'_>],
    targets: &HashMap<&str, usize>,
) -> Option<usize> {
    let mut target = source.get("target")?;
    for _ in 0..16 {
        let index = *targets.get(target)?;
        let entity = &entities[index];
        if classname(entity) != "target_relay" {
            return Some(index);
        }
        target = entity.get("target")?;
    }
    None
}

struct Bsp<'a> {
    entities: &'a [u8],
    shaders: Vec<Shader<'a>>,
    planes: Vec<Plane>,
    models: Vec<&'a [u8]>,
    brushes: Vec<&'a [u8]>,
    brush_sides: Vec<&'a [u8]>,
    vertices: Vec<Vertex>,
    indices: Vec<i32>,
    faces: Vec<&'a [u8]>,
    lightmaps: &'a [u8],
}

struct Shader<'a> {
    name: &'a str,
    surface_flags: i32,
    contents: i32,
}

#[derive(Clone, Copy)]
struct Vertex {
    position: Vec3,
    normal: Vec3,
    uv: Vec2,
    light_uv: Vec2,
}

impl<'a> Bsp<'a> {
    fn read(data: &'a [u8]) -> Result<Self, String> {
        if data.len() < HEADER_BYTES || !is_bsp(data) {
            return Err("Expected Quake III IBSP version 46".into());
        }
        let mut lumps = [&[][..]; 17];
        for (index, lump) in lumps.iter_mut().enumerate() {
            let start = nonnegative(int(data, 8 + index * 8), "lump offset")?;
            let len = nonnegative(int(data, 12 + index * 8), "lump length")?;
            let end = start
                .checked_add(len)
                .ok_or("Quake III lump range overflow")?;
            if len > 0 && start < HEADER_BYTES {
                return Err("Quake III lump overlaps its header".into());
            }
            *lump = data
                .get(start..end)
                .ok_or("Quake III lump outside the BSP")?;
        }
        let shaders = rows(lumps[1], 72, 16_384, "shaders")?
            .map(|row| {
                let end = row[..64].iter().position(|&b| b == 0).unwrap_or(64);
                Ok(Shader {
                    name: std::str::from_utf8(&row[..end])
                        .map_err(|_| "Invalid Quake III shader name")?,
                    surface_flags: int(row, 64),
                    contents: int(row, 68),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let planes = rows(lumps[2], 16, 262_144, "planes")?
            .map(|row| {
                let normal = source_axes(vector(row, 0));
                let distance = float(row, 12);
                if !normal.is_finite() || !distance.is_finite() || normal.length_squared() < 0.9 {
                    return Err("Invalid Quake III plane".into());
                }
                Ok(Plane { normal, distance })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let vertices = rows(lumps[10], 44, 1_000_000, "vertices")?
            .map(|row| {
                let vertex = Vertex {
                    position: source_axes(vector(row, 0)),
                    normal: source_axes(vector(row, 28)),
                    uv: pair(row, 12),
                    light_uv: pair(row, 20),
                };
                if !vertex.position.is_finite()
                    || !vertex.normal.is_finite()
                    || !vertex.uv.is_finite()
                {
                    return Err("Invalid Quake III vertex".into());
                }
                Ok(vertex)
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            entities: lumps[0],
            shaders,
            planes,
            models: rows(lumps[7], 40, 16_384, "models")?.collect(),
            brushes: rows(lumps[8], 12, 262_144, "brushes")?.collect(),
            brush_sides: rows(lumps[9], 8, 1_000_000, "brush sides")?.collect(),
            vertices,
            indices: rows(lumps[11], 4, 3_000_000, "indices")?
                .map(|row| int(row, 0))
                .collect(),
            faces: rows(lumps[13], 104, 262_144, "surfaces")?.collect(),
            lightmaps: lumps[14],
        })
    }

    fn brush(&self, row: &[u8], offset: Vec3) -> Result<Brush, String> {
        let sides = row_range(&self.brush_sides, row, 0, "brush sides")?;
        if !(4..=128).contains(&sides.len()) {
            return Err("Invalid Quake III brush plane count".into());
        }
        let mut planes = Vec::with_capacity(sides.len());
        let mut min = Vec3::splat(f32::NEG_INFINITY);
        let mut max = Vec3::splat(f32::INFINITY);
        for side in sides {
            let mut plane = *item(&self.planes, int(side, 0), "plane")?;
            plane.distance += plane.normal.dot(offset);
            for axis in 0..3 {
                if plane.normal[axis] == 1.0 {
                    max[axis] = max[axis].min(plane.distance);
                } else if plane.normal[axis] == -1.0 {
                    min[axis] = min[axis].max(-plane.distance);
                }
            }
            planes.push(plane);
        }
        if min.is_finite() && max.is_finite() && min.cmplt(max).all() {
            return Ok(Brush {
                planes: planes.into(),
                min,
                max,
            });
        }
        crate::source_bsp::convex_brush(&planes)
    }

    fn add_model(
        &self,
        model: &[u8],
        offset: Vec3,
        assets: &mut Assets<'_>,
        lightmaps: &[usize],
        brushes: &mut Vec<Brush>,
        surfaces: &mut Vec<Surface>,
        sky: &mut Option<[usize; 6]>,
    ) -> Result<(), String> {
        for row in row_range(&self.brushes, model, 32, "model brushes")? {
            let shader = item(&self.shaders, int(row, 8), "brush shader")?;
            if shader.contents & (CONTENTS_SOLID | CONTENTS_PLAYERCLIP) != 0 {
                brushes.push(self.brush(row, offset)?);
            }
        }
        for row in row_range(&self.faces, model, 24, "model surfaces")? {
            let shader = item(&self.shaders, int(row, 0), "surface shader")?;
            if shader.surface_flags & SURF_SKY != 0 {
                if sky.is_none() {
                    *sky = assets.sky(shader.name);
                }
                continue;
            }
            if shader.surface_flags & (SURF_NODRAW | SURF_SKIP) != 0 {
                continue;
            }
            let surface = Face {
                offset,
                texture: assets.material(shader.name),
                lightmap: usize::try_from(int(row, 28))
                    .ok()
                    .and_then(|index| lightmaps.get(index))
                    .copied(),
            };
            let vertices = row_range(&self.vertices, row, 12, "surface vertices")?;
            // q3map can leave lightmap coordinates uninitialized on vertex-lit patches.
            if surface.lightmap.is_some()
                && vertices.iter().any(|vertex| !vertex.light_uv.is_finite())
            {
                return Err("Invalid Quake III baked lightmap coordinates".into());
            }
            match int(row, 8) {
                1 | 3 => {
                    let indices = row_range(&self.indices, row, 20, "surface indices")?;
                    if indices.len() % 3 != 0 {
                        return Err("Invalid Quake III triangle index count".into());
                    }
                    for triangle in indices.chunks_exact(3) {
                        surface.emit(
                            [
                                *item(vertices, triangle[0], "surface vertex")?,
                                *item(vertices, triangle[1], "surface vertex")?,
                                *item(vertices, triangle[2], "surface vertex")?,
                            ],
                            surfaces,
                        )?;
                    }
                }
                2 => surface.patch(vertices, int(row, 96), int(row, 100), surfaces)?,
                4 => {} // Camera-facing flare.
                _ => return Err("Unsupported Quake III surface type".into()),
            }
        }
        Ok(())
    }
}

struct Face {
    offset: Vec3,
    texture: Option<usize>,
    lightmap: Option<usize>,
}

impl Face {
    fn emit(&self, mut vertices: [Vertex; 3], surfaces: &mut Vec<Surface>) -> Result<(), String> {
        let normal = (vertices[1].position - vertices[0].position)
            .cross(vertices[2].position - vertices[0].position);
        if normal.length_squared() <= 1e-8 {
            return Ok(());
        }
        if surfaces.len() >= MAX_TRIANGLES {
            return Err("Quake III map exceeds the tessellated triangle limit".into());
        }
        let authored = vertices.iter().map(|vertex| vertex.normal).sum::<Vec3>();
        if normal.dot(authored) < 0.0 {
            vertices.swap(1, 2);
        }
        surfaces.push(Surface {
            vertices: vertices
                .iter()
                .map(|vertex| vertex.position + self.offset)
                .collect(),
            color: [0.62, 0.62, 0.62],
            texture: self.texture.map(|index| SurfaceTexture {
                index,
                uvs: vertices.iter().map(|vertex| vertex.uv.to_array()).collect(),
            }),
            lightmap: self.lightmap.map(|index| SurfaceLightmap {
                index,
                uvs: vertices
                    .iter()
                    .map(|vertex| vertex.light_uv.to_array())
                    .collect(),
            }),
        });
        Ok(())
    }

    fn patch(
        &self,
        vertices: &[Vertex],
        width: i32,
        height: i32,
        surfaces: &mut Vec<Surface>,
    ) -> Result<(), String> {
        let width = nonnegative(width, "patch width")?;
        let height = nonnegative(height, "patch height")?;
        if width < 3
            || height < 3
            || width % 2 == 0
            || height % 2 == 0
            || width.checked_mul(height) != Some(vertices.len())
        {
            return Err("Invalid Quake III patch dimensions".into());
        }
        for y in (0..height - 2).step_by(2) {
            for x in (0..width - 2).step_by(2) {
                let linear = |a: Vertex, b: Vertex, c: Vertex| {
                    b.position
                        .abs_diff_eq((a.position + c.position) * 0.5, 0.0001)
                        && b.uv.abs_diff_eq((a.uv + c.uv) * 0.5, 0.0001)
                        && (self.lightmap.is_none()
                            || b.light_uv
                                .abs_diff_eq((a.light_uv + c.light_uv) * 0.5, 0.0001))
                };
                let corner = |dx, dy| vertices[(y + dy) * width + x + dx];
                // Extruded patches need subdivision only across the curved axis.
                let columns =
                    if (0..3).all(|row| linear(corner(0, row), corner(1, row), corner(2, row))) {
                        1
                    } else {
                        PATCH_STEPS
                    };
                let rows =
                    if (0..3).all(|col| linear(corner(col, 0), corner(col, 1), corner(col, 2))) {
                        1
                    } else {
                        PATCH_STEPS
                    };
                let mut samples = [[vertices[0]; PATCH_STEPS + 1]; PATCH_STEPS + 1];
                for (row, sample_row) in samples.iter_mut().take(rows + 1).enumerate() {
                    for (col, sample) in sample_row.iter_mut().take(columns + 1).enumerate() {
                        *sample = sample_patch(
                            vertices,
                            width,
                            x,
                            y,
                            col as f32 / columns as f32,
                            row as f32 / rows as f32,
                        );
                    }
                }
                for y in 0..rows {
                    for x in 0..columns {
                        let a = samples[y][x];
                        let b = samples[y][x + 1];
                        let c = samples[y + 1][x];
                        let d = samples[y + 1][x + 1];
                        self.emit([a, b, c], surfaces)?;
                        self.emit([b, d, c], surfaces)?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn sample_patch(vertices: &[Vertex], width: usize, x: usize, y: usize, u: f32, v: f32) -> Vertex {
    let weights = |t: f32| [(1.0 - t).powi(2), 2.0 * t * (1.0 - t), t * t];
    let mut result = Vertex {
        position: Vec3::ZERO,
        normal: Vec3::ZERO,
        uv: Vec2::ZERO,
        light_uv: Vec2::ZERO,
    };
    for (dy, vertical) in weights(v).into_iter().enumerate() {
        for (dx, horizontal) in weights(u).into_iter().enumerate() {
            let weight = vertical * horizontal;
            let vertex = vertices[(y + dy) * width + x + dx];
            result.position += vertex.position * weight;
            result.normal += vertex.normal * weight;
            result.uv += vertex.uv * weight;
            result.light_uv += vertex.light_uv * weight;
        }
    }
    result
}

fn rows<'a>(
    data: &'a [u8],
    size: usize,
    limit: usize,
    label: &str,
) -> Result<std::slice::ChunksExact<'a, u8>, String> {
    if data.len() % size != 0 || data.len() / size > limit {
        return Err(format!("Invalid Quake III {label} lump size"));
    }
    Ok(data.chunks_exact(size))
}

fn row_range<'a, T>(
    values: &'a [T],
    row: &[u8],
    index: usize,
    label: &str,
) -> Result<&'a [T], String> {
    let start = nonnegative(int(row, index), label)?;
    let count = nonnegative(int(row, index + 4), label)?;
    let end = start.checked_add(count).ok_or("Quake III range overflow")?;
    values
        .get(start..end)
        .ok_or_else(|| format!("Invalid Quake III {label} range"))
}

fn item<'a, T>(values: &'a [T], index: i32, label: &str) -> Result<&'a T, String> {
    values
        .get(nonnegative(index, label)?)
        .ok_or_else(|| format!("Invalid Quake III {label} index"))
}

fn nonnegative(value: i32, label: &str) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("Negative Quake III {label}"))
}

fn int(row: &[u8], index: usize) -> i32 {
    i32::from_le_bytes(row[index..index + 4].try_into().unwrap())
}

fn float(row: &[u8], index: usize) -> f32 {
    f32::from_bits(int(row, index) as u32)
}

fn vector(row: &[u8], index: usize) -> Vec3 {
    Vec3::new(
        float(row, index),
        float(row, index + 4),
        float(row, index + 8),
    )
}

fn pair(row: &[u8], index: usize) -> Vec2 {
    Vec2::new(float(row, index), float(row, index + 4))
}

struct Assets<'a> {
    pack: Option<&'a ZipReader<'a>>,
    entries: HashMap<String, usize>,
    roots: Vec<std::path::PathBuf>,
    shaders: HashMap<String, ShaderScript>,
    materials: HashMap<String, Option<usize>>,
    images: HashMap<String, usize>,
    textures: Vec<CourseTexture>,
    decoded_bytes: usize,
}

#[derive(Default)]
struct ShaderScript {
    image: Option<String>,
    editor_image: Option<String>,
    skybox: Option<String>,
}

impl<'a> Assets<'a> {
    fn new(pack: Option<&'a ZipReader<'a>>, map_path: &Path) -> Self {
        let entries: HashMap<String, usize> = pack
            .into_iter()
            .flat_map(ZipReader::entries)
            .enumerate()
            .filter(|(_, entry)| !entry.path_is_unsafe())
            .filter_map(|(index, entry)| Some((asset_name(&entry.path)?, index)))
            .collect();
        let mut roots = Vec::new();
        if let Some(parent) = map_path.parent() {
            roots.push(parent.to_owned());
            if parent
                .file_name()
                .is_some_and(|part| part.eq_ignore_ascii_case("maps"))
                && let Some(root) = parent.parent()
            {
                roots.push(root.to_owned());
            }
        }
        if let Some(value) = std::env::var_os("SURFD_QUAKE3_ASSETS") {
            roots.extend(std::env::split_paths(&value));
        }
        let mut assets = Self {
            pack,
            entries,
            roots,
            shaders: HashMap::new(),
            materials: HashMap::new(),
            images: HashMap::new(),
            textures: Vec::new(),
            decoded_bytes: 0,
        };
        let mut scripts: Vec<_> = assets
            .entries
            .keys()
            .filter(|name| name.starts_with("scripts/") && name.ends_with(".shader"))
            .cloned()
            .collect();
        scripts.sort();
        scripts.truncate(128);
        for path in scripts {
            if let Some(bytes) = assets.packed_file(&path, 512 * 1024) {
                assets
                    .shaders
                    .extend(shader_scripts(&String::from_utf8_lossy(&bytes)));
            }
        }
        assets
    }

    fn packed_file(&self, name: &str, limit: u64) -> Option<Cow<'a, [u8]>> {
        let pack = self.pack?;
        let entry = pack.entries().get(*self.entries.get(name)?)?;
        pack.entry_bytes(
            entry,
            &Limits {
                max_entry_bytes: limit,
                ..Limits::default()
            },
        )
        .ok()
    }

    fn image_file(&self, name: &str) -> Option<Cow<'a, [u8]>> {
        use std::io::Read as _;

        if let Some(bytes) = self.packed_file(name, MAX_ASSET_BYTES) {
            return Some(bytes);
        }
        self.roots.iter().find_map(|root| {
            let file = std::fs::File::open(root.join(name)).ok()?;
            if file.metadata().ok()?.len() > MAX_ASSET_BYTES {
                return None;
            }
            let mut bytes = Vec::new();
            file.take(MAX_ASSET_BYTES + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() as u64 <= MAX_ASSET_BYTES).then_some(Cow::Owned(bytes))
        })
    }

    fn material(&mut self, name: &str) -> Option<usize> {
        if let Some(&index) = self.materials.get(name) {
            return index;
        }
        let key = asset_name(name)?;
        let image = self
            .shaders
            .get(&key)
            .and_then(|shader| shader.image.clone());
        let editor = self
            .shaders
            .get(&key)
            .and_then(|shader| shader.editor_image.clone());
        let index = image
            .as_deref()
            .and_then(|path| self.image(path))
            .or_else(|| self.image(&key))
            .or_else(|| editor.as_deref().and_then(|path| self.image(path)));
        self.materials.insert(name.to_owned(), index);
        index
    }

    fn image(&mut self, name: &str) -> Option<usize> {
        let path = asset_name(name)?;
        let base = path
            .rsplit_once('.')
            .filter(|(_, ext)| matches!(*ext, "tga" | "jpg" | "jpeg" | "png"))
            .map_or(path.as_str(), |(base, _)| base);
        if let Some(&index) = self.images.get(base) {
            return Some(index);
        }
        let candidates = [
            path.clone(),
            format!("{base}.tga"),
            format!("{base}.jpg"),
            format!("{base}.jpeg"),
            format!("{base}.png"),
        ];
        for file in candidates {
            let Some(bytes) = self.image_file(&file) else {
                continue;
            };
            let Some((width, height, rgba)) = decode_image(&file, &bytes) else {
                continue;
            };
            let alpha = if rgba.chunks_exact(4).all(|pixel| pixel[3] == 255) {
                TextureAlpha::Opaque
            } else if rgba
                .chunks_exact(4)
                .all(|pixel| matches!(pixel[3], 0 | 255))
            {
                TextureAlpha::Cutout
            } else {
                TextureAlpha::Blend
            };
            let index = self.store(file, width, height, rgba, alpha)?;
            self.images.insert(base.to_owned(), index);
            return Some(index);
        }
        None
    }

    fn lightmaps(&mut self, data: &[u8]) -> Result<Vec<usize>, String> {
        rows(data, LIGHTMAP_BYTES, 1024, "lightmaps")?
            .enumerate()
            .map(|(index, rgb)| {
                let mut rgba = Vec::with_capacity(LIGHTMAP_EDGE * LIGHTMAP_EDGE * 4);
                for pixel in rgb.chunks_exact(3) {
                    rgba.extend_from_slice(pixel);
                    rgba.push(255);
                }
                self.store(
                    format!("Quake III lightmap {index}"),
                    LIGHTMAP_EDGE as u32,
                    LIGHTMAP_EDGE as u32,
                    rgba,
                    TextureAlpha::Opaque,
                )
                .ok_or_else(|| "Quake III lightmap memory limit exceeded".into())
            })
            .collect()
    }

    fn sky(&mut self, shader: &str) -> Option<[usize; 6]> {
        let skybox = self.shaders.get(&asset_name(shader)?)?.skybox.clone()?;
        let mut sky = [0; 6];
        for (index, suffix) in ["rt", "lf", "up", "dn", "ft", "bk"].iter().enumerate() {
            sky[index] = self.image(&format!("{skybox}_{suffix}"))?;
        }
        let edge = self.textures[sky[0]].width;
        if sky.iter().any(|&index| {
            let texture = &self.textures[index];
            texture.width != edge || texture.height != edge
        }) {
            return None;
        }
        let edge = edge as usize;
        // Quake III's file UVs use Z-up. Convert to the GPU cube layout in Y-up.
        for (face, index) in sky.into_iter().enumerate() {
            let rgba = &mut self.textures[index].rgba;
            let original = rgba.clone();
            for y in 0..edge {
                for x in 0..edge {
                    let (u, v) = match face {
                        2 => (y, x),
                        3 => (edge - 1 - y, edge - 1 - x),
                        _ => (edge - 1 - x, y),
                    };
                    let source = (v * edge + u) * 4;
                    let destination = (y * edge + x) * 4;
                    rgba[destination..destination + 4]
                        .copy_from_slice(&original[source..source + 4]);
                }
            }
        }
        Some(sky)
    }

    fn store(
        &mut self,
        name: String,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        alpha: TextureAlpha,
    ) -> Option<usize> {
        let total = self.decoded_bytes.checked_add(rgba.len())?;
        if total > MAX_IMAGE_BYTES {
            return None;
        }
        self.decoded_bytes = total;
        let index = self.textures.len();
        self.textures.push(CourseTexture {
            name,
            width,
            height,
            rgba,
            bc: None,
            alpha,
        });
        Some(index)
    }
}

fn asset_name(name: &str) -> Option<String> {
    let path = name
        .trim_matches('"')
        .replace('\\', "/")
        .to_ascii_lowercase();
    if path.starts_with('$')
        || path
            .split('/')
            .any(|part| matches!(part, "" | "." | "..") || part.contains([':', '\0']))
    {
        return None;
    }
    Some(path)
}

fn shader_scripts(source: &str) -> HashMap<String, ShaderScript> {
    let source = source
        .lines()
        .map(|line| line.split("//").next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
        .replace('{', " { ")
        .replace('}', " } ");
    let mut scripts = HashMap::new();
    let mut depth = 0usize;
    let mut name = None;
    let mut script = ShaderScript::default();
    let mut tokens = source.split_ascii_whitespace();
    while let Some(token) = tokens.next() {
        match token {
            "{" => depth += 1,
            "}" => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some(name) = name.take()
                {
                    scripts.insert(name, std::mem::take(&mut script));
                }
            }
            token if depth == 0 => name = asset_name(token),
            token if depth == 1 && token.eq_ignore_ascii_case("qer_editorimage") => {
                script.editor_image = tokens.next().and_then(asset_name);
            }
            token if depth == 1 && token.eq_ignore_ascii_case("skyparms") => {
                script.skybox = tokens
                    .next()
                    .filter(|&value| value != "-")
                    .and_then(asset_name);
            }
            token
                if depth == 2
                    && (token.eq_ignore_ascii_case("map")
                        || token.eq_ignore_ascii_case("clampmap")) =>
            {
                let image = tokens.next().and_then(asset_name);
                if script.image.is_none() {
                    script.image = image;
                }
            }
            _ => {}
        }
    }
    scripts
}

fn decode_image(path: &str, data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if path.ends_with(".tga") {
        return decode_tga(data);
    }
    let mut reader = image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(MAX_ASSET_BYTES);
    reader.limits(limits);
    let rgba = reader.decode().ok()?.into_rgba8();
    Some((rgba.width(), rgba.height(), rgba.into_raw()))
}

fn decode_tga(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let header = data.get(..18)?;
    let width = usize::from(u16::from_le_bytes([header[12], header[13]]));
    let height = usize::from(u16::from_le_bytes([header[14], header[15]]));
    if header[1] != 0
        || !(1..=4096).contains(&width)
        || !(1..=4096).contains(&height)
        || !matches!(header[2], 2 | 10)
        || !matches!(header[16], 24 | 32)
    {
        return None;
    }
    let pixel_bytes = usize::from(header[16] / 8);
    let top = header[17] & 0x20 != 0;
    let right = header[17] & 0x10 != 0;
    let rle = header[2] == 10;
    let alpha = pixel_bytes == 4 && header[17] & 0x0f != 0;
    let mut input = data.get(18 + usize::from(header[0])..)?;
    let mut rgba = vec![0; width * height * 4];
    let mut pixel = 0;
    while pixel < width * height {
        let (count, repeat) = if rle {
            let (&packet, remaining) = input.split_first()?;
            input = remaining;
            (usize::from(packet & 127) + 1, packet & 128 != 0)
        } else {
            (width * height, false)
        };
        if pixel + count > width * height {
            return None;
        }
        let read_bytes = pixel_bytes * if repeat { 1 } else { count };
        let (payload, remaining) = input.split_at_checked(read_bytes)?;
        input = remaining;
        for offset in 0..count {
            let source = pixel_bytes * if repeat { 0 } else { offset };
            let x = pixel % width;
            let y = pixel / width;
            let x = if right { width - 1 - x } else { x };
            let y = if top { y } else { height - 1 - y };
            let destination = (y * width + x) * 4;
            rgba[destination..destination + 4].copy_from_slice(&[
                payload[source + 2],
                payload[source + 1],
                payload[source],
                if alpha { payload[source + 3] } else { 255 },
            ]);
            pixel += 1;
        }
    }
    Some((width as u32, height as u32, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_quake3_headers_and_lump_ranges() {
        let mut data = vec![0; HEADER_BYTES];
        data[..8].copy_from_slice(b"IBSP\x2e\x00\x00\x00");
        assert!(Bsp::read(&data).is_ok());
        data[8..12].copy_from_slice(&(HEADER_BYTES as i32).to_le_bytes());
        data[12..16].copy_from_slice(&1_i32.to_le_bytes());
        assert!(Bsp::read(&data).is_err());
        data[4] = 47;
        assert!(!is_bsp(&data));
    }

    #[test]
    fn tga_rle_respects_top_origin_and_transparency() {
        let mut data = vec![0; 18];
        data[2] = 10;
        data[12] = 2;
        data[14] = 1;
        data[16] = 32;
        data[17] = 0x28;
        data.extend([0x81, 7, 13, 255, 64]);
        let (width, height, rgba) = decode_tga(&data).unwrap();
        assert_eq!((width, height), (2, 1));
        assert_eq!(rgba, [255, 13, 7, 64, 255, 13, 7, 64]);
        data[18] = 0x82;
        assert!(decode_tga(&data).is_none());
    }

    #[test]
    #[ignore = "requires the original DeFRaG competition map PK3"]
    fn loads_original_defrag_map() {
        let path = std::env::var_os("SURFD_TEST_DEFRAG_PK3").expect("set SURFD_TEST_DEFRAG_PK3");
        let course = load(Path::new(&path)).unwrap();
        assert_eq!(course.name, "dfwc2021-3");
        assert!(course.catalog_revision.is_none());
        assert!(course.brushes.len() > 5_000);
        assert!(course.surfaces.len() > 25_000);
        assert!(
            course
                .surfaces
                .iter()
                .any(|face| face.texture.is_some() && face.lightmap.is_some())
        );
        assert!(course.sky.is_some());
        assert!(
            course
                .zones
                .iter()
                .any(|zone| matches!(zone.kind, ZoneKind::Start))
        );
        assert!(
            course
                .zones
                .iter()
                .any(|zone| matches!(zone.kind, ZoneKind::Finish))
        );
        for checkpoint in 1..=3 {
            assert!(course.zones.iter().any(
                |zone| matches!(zone.kind, ZoneKind::Checkpoint(index) if index == checkpoint)
            ));
        }
        eprintln!(
            "{} collision brushes, {} triangles, {} textures, {} zones",
            course.brushes.len(),
            course.surfaces.len(),
            course.textures.len(),
            course.zones.len()
        );
    }
}
