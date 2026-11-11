//! Reads the static geometry and basic entities of little-endian Source 1 maps.
//!
//! Lump layouts follow Valve's Source SDK `bspfile.h` and `bspflags.h`.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

use bevy_math::{Quat, Vec3};
use lzma_rs::decompress::{Options, UnpackedSize};

use crate::course::{Brush, Course, Plane, Surface, SurfaceTexture, Zone, ZoneKind};
use crate::source_material::Materials;

#[path = "source_lightmap.rs"]
mod lightmap;
use lightmap::{FaceLightmap, Lightmaps};

const HEADER_SIZE: usize = 8 + 64 * 16 + 4;
const MAX_FILE_SIZE: u64 = 512 * 1024 * 1024;
const MAX_LUMP_SIZE: usize = 64 * 1024 * 1024;
const PLAYER_SOLID: i32 = 0x1 | 0x2 | 0x8 | 0x4000 | 0x10000;
const HIDDEN_SURFACE: i32 = 0x2 | 0x4 | 0x40 | 0x80 | 0x100 | 0x200;

const ENTITIES: usize = 0;
const PLANES: usize = 1;
const TEXDATA: usize = 2;
const VERTICES: usize = 3;
const NODES: usize = 5;
const TEXINFO: usize = 6;
const FACES: usize = 7;
const LIGHTING: usize = 8;
const LEAVES: usize = 10;
const EDGES: usize = 12;
const SURFEDGES: usize = 13;
const MODELS: usize = 14;
const LEAF_BRUSHES: usize = 17;
const BRUSHES: usize = 18;
const BRUSH_SIDES: usize = 19;
const PAKFILE: usize = 40;
const TEXDATA_STRING_DATA: usize = 43;
const TEXDATA_STRING_TABLE: usize = 44;
const LIGHTING_HDR: usize = 53;
const FACES_HDR: usize = 58;

pub fn load(path: &Path) -> Result<Course, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_FILE_SIZE + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_FILE_SIZE {
        return Err(format!("{} exceeds the 512 MiB BSP limit", path.display()));
    }
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Source BSP");
    parse(&bytes, name, Some(path)).map_err(|error| format!("{}: {error}", path.display()))
}

fn parse(bytes: &[u8], name: &str, map_path: Option<&Path>) -> Result<Course, String> {
    let file = BspFile::new(bytes)?;
    let geometry = Geometry::read(&file)?;
    let pak = file.lump(PAKFILE).unwrap_or_default();
    let mut materials = Materials::new(&pak, map_path);
    let lighting = file.lump(geometry.lighting).unwrap_or_default();
    let mut lightmaps = Lightmaps::new(&lighting);
    let entity_lump = file.lump(ENTITIES)?;
    let entity_text = String::from_utf8_lossy(&entity_lump);
    let entities = read_entities(&entity_text)?;
    let (spawn, spawn_yaw) = [
        "info_player_counterterrorist",
        "info_player_terrorist",
        "info_player_start",
        "info_player_deathmatch",
    ]
    .iter()
    .find_map(|class| {
        entities.iter().find_map(|entity| {
            (entity.get("classname") == Some(*class))
                .then(|| entity.origin().map(|origin| (origin, entity.yaw())))
                .flatten()
        })
    })
    .ok_or("The map has no usable player spawn")?;

    let world = geometry
        .models
        .first()
        .ok_or("The BSP has no world model")?;
    let mut brushes = geometry.model_brushes(world, Placement::IDENTITY, true)?;
    let mut surfaces =
        geometry.model_surfaces(world, Placement::IDENTITY, &mut materials, &mut lightmaps)?;
    let mut zones = Vec::new();
    for entity in &entities {
        let Some(model_index) = entity.model_index().filter(|&index| index != 0) else {
            continue;
        };
        let model = lookup(&geometry.models, model_index, "brush model")?;
        if entity.get("startdisabled") == Some("1") {
            continue;
        }

        let class = entity.get("classname").unwrap_or_default();
        let placement = entity.placement();
        if let Some(kind) = zone_kind(entity, &entities) {
            zones.extend(
                geometry
                    .model_brushes(model, placement, false)?
                    .into_iter()
                    .map(|brush| Zone {
                        brush,
                        kind: kind.clone(),
                    }),
            );
        } else if matches!(
            class,
            "func_brush" | "func_wall" | "func_illusionary" | "func_lod"
        ) {
            surfaces.extend(geometry.model_surfaces(
                model,
                placement,
                &mut materials,
                &mut lightmaps,
            )?);
            if class != "func_illusionary" && entity.get("solidity") != Some("1") {
                brushes.extend(geometry.model_brushes(model, placement, true)?);
            }
        }
    }

    if brushes.is_empty() || surfaces.is_empty() {
        return Err("The map has no supported static brush geometry".into());
    }
    let kill_y = brushes
        .iter()
        .map(|brush| brush.min.y)
        .fold(spawn.y, f32::min)
        - 2048.0;
    let mut textures = materials.into_textures();
    lightmaps.append(&mut textures, &mut surfaces);
    Ok(Course {
        name: name.to_owned(),
        spawn,
        spawn_yaw,
        brushes,
        surfaces,
        textures,
        visual_meshes: Vec::new(),
        visual_visibility: None,
        sky: None,
        sky_occluders: Vec::new(),
        fog: None,
        zones,
        kill_y,
        practice_spawns: Vec::new(),
        catalog_revision: None,
        bonus_routes: Vec::new(),
        map_triggers: Default::default(),
    })
}

struct BspFile<'a> {
    bytes: &'a [u8],
}

impl<'a> BspFile<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_SIZE || &bytes[..4] != b"VBSP" {
            return Err("Expected a Source 1 VBSP file".into());
        }
        let version = i32_at(bytes, 4);
        if !matches!(version, 19..=21) {
            return Err(format!(
                "Unsupported VBSP version {version}. Expected 19, 20 or 21"
            ));
        }
        Ok(Self { bytes })
    }

    fn entry(&self, index: usize) -> &[u8] {
        let start = 8 + index * 16;
        &self.bytes[start..start + 16]
    }

    fn lump(&self, index: usize) -> Result<Cow<'a, [u8]>, String> {
        let entry = self.entry(index);
        let offset = u32_at(entry, 0) as usize;
        let length = u32_at(entry, 4) as usize;
        let unpacked = u32_at(entry, 12) as usize;
        if length == 0 && unpacked == 0 {
            return Ok(Cow::Borrowed(&[]));
        }
        let stored_limit = if index == PAKFILE && unpacked == 0 {
            MAX_FILE_SIZE as usize
        } else {
            MAX_LUMP_SIZE
        };
        let data = self
            .bytes
            .get(offset..offset.saturating_add(length))
            .filter(|_| offset >= HEADER_SIZE && length <= stored_limit)
            .ok_or_else(|| format!("BSP lump {index} has an invalid range"))?;
        if unpacked == 0 {
            return Ok(Cow::Borrowed(data));
        }
        if data.len() < 17
            || &data[..4] != b"LZMA"
            || unpacked > MAX_LUMP_SIZE
            || u32_at(data, 4) as usize != unpacked
            || u32_at(data, 8) as usize != data.len() - 17
        {
            return Err(format!("BSP lump {index} has an invalid LZMA header"));
        }
        // Valve stores the five LZMA properties followed by raw compressed bytes.
        let mut output = Vec::new();
        lzma_rs::lzma_decompress_with_options(
            &mut Cursor::new(&data[12..]),
            &mut output,
            &Options {
                unpacked_size: UnpackedSize::UseProvided(Some(unpacked as u64)),
                memlimit: Some(MAX_LUMP_SIZE),
                ..Options::default()
            },
        )
        .map_err(|error| format!("Could not unpack BSP lump {index}: {error}"))?;
        if output.len() != unpacked {
            return Err(format!("BSP lump {index} decompressed to the wrong size"));
        }
        Ok(Cow::Owned(output))
    }

    fn records<const WIDTH: usize, T>(
        &self,
        index: usize,
        mut read: impl FnMut(&[u8]) -> T,
    ) -> Result<Vec<T>, String> {
        let bytes = self.lump(index)?;
        let (records, remainder) = bytes.as_chunks::<WIDTH>();
        if !remainder.is_empty() {
            return Err(format!(
                "BSP lump {index} has a partial {WIDTH}-byte record"
            ));
        }
        Ok(records.iter().map(|record| read(record)).collect())
    }
}

struct Model {
    root: i32,
    first_face: usize,
    face_count: usize,
}

struct RawBrush {
    first_side: usize,
    side_count: usize,
    contents: i32,
}

struct Face {
    plane: usize,
    reversed: bool,
    first_edge: usize,
    edge_count: usize,
    texture: i16,
    displacement: i16,
    lightmap: FaceLightmap,
}

struct TextureInfo {
    axes: [(Vec3, f32); 2],
    light_axes: [(Vec3, f32); 2],
    flags: i32,
    data: usize,
}

struct TextureData {
    name: Option<String>,
    size: [f32; 2],
    color: [f32; 3],
}

struct Geometry {
    planes: Vec<Plane>,
    nodes: Vec<[i32; 2]>,
    leaves: Vec<[usize; 2]>,
    leaf_brushes: Vec<usize>,
    models: Vec<Model>,
    brushes: Vec<RawBrush>,
    sides: Vec<usize>,
    vertices: Vec<Vec3>,
    edges: Vec<[usize; 2]>,
    face_edges: Vec<i32>,
    faces: Vec<Face>,
    textures: Vec<TextureInfo>,
    materials: Vec<TextureData>,
    lighting: usize,
}

impl Geometry {
    fn read(file: &BspFile<'_>) -> Result<Self, String> {
        let leaf = |row: &[u8]| [u16_at(row, 24), u16_at(row, 26)];
        let leaves = match i32_at(file.entry(LEAVES), 8) {
            0 => file.records::<56, _>(LEAVES, leaf)?,
            1 => file.records::<32, _>(LEAVES, leaf)?,
            version => return Err(format!("Unsupported Source leaf version {version}")),
        };
        let has_lump = |index| u32_at(file.entry(index), 4) != 0;
        let (faces_lump, lighting) = if !has_lump(FACES)
            || (!has_lump(LIGHTING) && has_lump(FACES_HDR) && has_lump(LIGHTING_HDR))
        {
            (FACES_HDR, LIGHTING_HDR)
        } else {
            (FACES, LIGHTING)
        };
        let material_names = file.lump(TEXDATA_STRING_DATA).unwrap_or_default();
        let material_offsets = file
            .records::<4, _>(TEXDATA_STRING_TABLE, |row| u32_at(row, 0) as usize)
            .unwrap_or_default();
        Ok(Self {
            planes: file.records::<20, _>(PLANES, |row| Plane {
                normal: source_axes(vector_at(row, 0)),
                distance: f32_at(row, 12),
            })?,
            nodes: file.records::<32, _>(NODES, |row| [i32_at(row, 4), i32_at(row, 8)])?,
            leaves,
            leaf_brushes: file.records::<2, _>(LEAF_BRUSHES, |row| u16_at(row, 0))?,
            models: file.records::<48, _>(MODELS, |row| Model {
                root: i32_at(row, 36),
                first_face: u32_at(row, 40) as usize,
                face_count: u32_at(row, 44) as usize,
            })?,
            brushes: file.records::<12, _>(BRUSHES, |row| RawBrush {
                first_side: u32_at(row, 0) as usize,
                side_count: u32_at(row, 4) as usize,
                contents: i32_at(row, 8),
            })?,
            sides: file.records::<8, _>(BRUSH_SIDES, |row| u16_at(row, 0))?,
            vertices: file.records::<12, _>(VERTICES, |row| source_axes(vector_at(row, 0)))?,
            edges: file.records::<4, _>(EDGES, |row| [u16_at(row, 0), u16_at(row, 2)])?,
            face_edges: file.records::<4, _>(SURFEDGES, |row| i32_at(row, 0))?,
            faces: file.records::<56, _>(faces_lump, |row| Face {
                plane: u16_at(row, 0),
                reversed: row[2] != 0,
                first_edge: u32_at(row, 4) as usize,
                edge_count: u16_at(row, 8),
                texture: i16_at(row, 10),
                displacement: i16_at(row, 12),
                lightmap: FaceLightmap {
                    offset: i32_at(row, 20),
                    mins: [i32_at(row, 28), i32_at(row, 32)],
                    extents: [i32_at(row, 36), i32_at(row, 40)],
                },
            })?,
            textures: file.records::<72, _>(TEXINFO, |row| TextureInfo {
                axes: [0, 16].map(|offset| texture_axis(row, offset)),
                light_axes: [32, 48].map(|offset| texture_axis(row, offset)),
                flags: i32_at(row, 64),
                data: u32_at(row, 68) as usize,
            })?,
            materials: file.records::<32, _>(TEXDATA, |row| TextureData {
                name: material_offsets
                    .get(u32_at(row, 12) as usize)
                    .and_then(|&offset| material_name(&material_names, offset)),
                size: [i32_at(row, 16) as f32, i32_at(row, 20) as f32],
                color: vector_at(row, 0).to_array().map(|value| {
                    if value.is_finite() {
                        value.clamp(0.035, 1.0).powf(1.0 / 2.2)
                    } else {
                        0.55
                    }
                }),
            })?,
            lighting,
        })
    }

    fn model_brushes(
        &self,
        model: &Model,
        placement: Placement,
        solid_only: bool,
    ) -> Result<Vec<Brush>, String> {
        let mut pending = vec![model.root];
        let mut visited = BTreeSet::new();
        let mut brush_indices = BTreeSet::new();
        while let Some(index) = pending.pop() {
            if !visited.insert(index) {
                continue;
            }
            if index >= 0 {
                pending.extend(lookup(&self.nodes, index as usize, "node")?);
            } else {
                let &[first, count] = lookup(&self.leaves, (!index) as usize, "leaf")?;
                brush_indices.extend(records(&self.leaf_brushes, first, count, "leaf brush")?);
            }
        }

        let mut brushes = Vec::new();
        for &index in brush_indices {
            let brush = lookup(&self.brushes, index, "brush")?;
            if solid_only && brush.contents & PLAYER_SOLID == 0 {
                continue;
            }
            let sides = records(
                &self.sides,
                brush.first_side,
                brush.side_count,
                "brush side",
            )?;
            let planes = sides
                .iter()
                .map(|&side| lookup(&self.planes, side, "brush plane"));
            brushes.push(brush_from_planes(planes, placement)?);
        }
        Ok(brushes)
    }

    fn model_surfaces(
        &self,
        model: &Model,
        placement: Placement,
        materials: &mut Materials<'_>,
        lightmaps: &mut Lightmaps<'_>,
    ) -> Result<Vec<Surface>, String> {
        let mut surfaces = Vec::new();
        for face in records(
            &self.faces,
            model.first_face,
            model.face_count,
            "model face",
        )? {
            if face.texture < 0 || face.displacement >= 0 || face.edge_count < 3 {
                continue;
            }
            let info = lookup(&self.textures, face.texture as usize, "face texture")?;
            if info.flags & HIDDEN_SURFACE != 0 {
                continue;
            }
            let material = self.materials.get(info.data);
            let color = material.map_or([0.53, 0.62, 0.69], |material| material.color);
            let mut vertices = Vec::with_capacity(face.edge_count);
            for &edge_index in records(
                &self.face_edges,
                face.first_edge,
                face.edge_count,
                "face edge",
            )? {
                let edge = lookup(&self.edges, edge_index.unsigned_abs() as usize, "edge")?;
                let vertex = lookup(&self.vertices, edge[usize::from(edge_index < 0)], "vertex")?;
                if !vertex.is_finite() {
                    return Err("BSP face has a non-finite vertex".into());
                }
                vertices.push(*vertex);
            }

            let normal = lookup(&self.planes, face.plane, "face plane")?.normal;
            let normal = if face.reversed { -normal } else { normal };
            let origin = vertices[0];
            let area: Vec3 = vertices[1..]
                .windows(2)
                .map(|edge| (edge[0] - origin).cross(edge[1] - origin))
                .sum();
            if area.length_squared() <= f32::EPSILON {
                continue;
            }
            if area.dot(normal) < 0.0 {
                vertices.reverse();
            }
            let resolved = material
                .and_then(|material| material.name.as_deref())
                .and_then(|name| materials.resolve(name));
            let lightmap = if resolved.is_some_and(|texture| !texture.lightmapped) {
                None
            } else {
                lightmaps.surface(face.lightmap, info.light_axes, &vertices)
            };
            let texture = material.and_then(|material| {
                if material.size.iter().any(|&dimension| dimension <= 0.0) {
                    return None;
                }
                let texture = resolved?;
                let uvs: Vec<_> = vertices
                    .iter()
                    .map(|&vertex| {
                        let [(s, s_offset), (t, t_offset)] = info.axes;
                        let [width, height] = material.size;
                        texture.uv([
                            (vertex.dot(s) + s_offset) / width,
                            (vertex.dot(t) + t_offset) / height,
                        ])
                    })
                    .collect();
                uvs.iter()
                    .flatten()
                    .all(|value| value.is_finite())
                    .then_some(SurfaceTexture {
                        index: texture.index,
                        uvs,
                    })
            });
            for vertex in &mut vertices {
                *vertex = placement.point(*vertex);
            }
            surfaces.push(Surface {
                vertices,
                color,
                texture,
                lightmap,
            });
        }
        Ok(surfaces)
    }
}

fn material_name(bytes: &[u8], offset: usize) -> Option<String> {
    let name = bytes.get(offset..)?;
    let end = name.iter().take(4096).position(|&byte| byte == 0)?;
    let name = std::str::from_utf8(&name[..end]).ok()?;
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

fn texture_axis(row: &[u8], offset: usize) -> (Vec3, f32) {
    (
        source_axes(vector_at(row, offset)),
        f32_at(row, offset + 12),
    )
}

#[derive(Clone, Copy)]
struct Placement {
    translation: Vec3,
    rotation: Quat,
}

impl Placement {
    const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
    };

    fn point(self, point: Vec3) -> Vec3 {
        self.rotation * point + self.translation
    }
}

fn brush_from_planes<'a>(
    source_planes: impl IntoIterator<Item = Result<&'a Plane, String>>,
    placement: Placement,
) -> Result<Brush, String> {
    let mut planes = Vec::new();
    let mut min = Vec3::splat(f32::NEG_INFINITY);
    let mut max = Vec3::splat(f32::INFINITY);
    for plane in source_planes {
        let plane = plane?;
        let length = plane.normal.length();
        if !length.is_finite() || length < 0.5 || !plane.distance.is_finite() {
            return Err("BSP brush contains an invalid plane".into());
        }
        let normal = plane.normal / length;
        let distance = plane.distance / length;
        // VBSP adds axial and edge bevels for tracing the player's axis-aligned hull.
        for (axis, basis) in [Vec3::X, Vec3::Y, Vec3::Z].into_iter().enumerate() {
            if normal == basis {
                max[axis] = max[axis].min(distance);
            } else if normal == -basis {
                min[axis] = min[axis].max(-distance);
            }
        }
        let normal = placement.rotation * normal;
        planes.push(Plane {
            normal,
            distance: distance + normal.dot(placement.translation),
        });
    }
    if !min.is_finite() || !max.is_finite() || !min.cmple(max).all() {
        return Err("BSP brush is missing valid axial bounds".into());
    }

    let center = placement.point((min + max) * 0.5);
    let half = (max - min) * 0.5;
    let extent = (placement.rotation * Vec3::X).abs() * half.x
        + (placement.rotation * Vec3::Y).abs() * half.y
        + (placement.rotation * Vec3::Z).abs() * half.z;
    Ok(Brush {
        planes: planes.into(),
        min: center - extent,
        max: center + extent,
    })
}

struct Entity<'a>(HashMap<String, &'a str>);

impl<'a> Entity<'a> {
    fn get(&self, key: &str) -> Option<&'a str> {
        self.0.get(key).copied()
    }

    fn origin(&self) -> Option<Vec3> {
        parse_vector(self.get("origin")?).map(source_axes)
    }

    fn angles(&self) -> Vec3 {
        if let Some(angles) = self.get("angles").and_then(parse_vector) {
            return angles;
        }
        let yaw = self
            .get("angle")
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite())
            .unwrap_or_default();
        Vec3::new(0.0, yaw, 0.0)
    }

    fn yaw(&self) -> f32 {
        (self.angles().y.rem_euclid(360.0) - 90.0).to_radians()
    }

    fn model_index(&self) -> Option<usize> {
        self.get("model")?.strip_prefix('*')?.parse().ok()
    }

    fn placement(&self) -> Placement {
        let angles = self.angles().map(f32::to_radians);
        Placement {
            translation: self.origin().unwrap_or_default(),
            rotation: Quat::from_rotation_y(angles.y)
                * Quat::from_rotation_z(-angles.x)
                * Quat::from_rotation_x(angles.z),
        }
    }
}

fn zone_kind(entity: &Entity<'_>, entities: &[Entity<'_>]) -> Option<ZoneKind> {
    let class = entity.get("classname")?;
    if !class.starts_with("trigger_") {
        return None;
    }
    let name = entity
        .get("targetname")
        .unwrap_or_default()
        .to_ascii_lowercase();
    match name.as_str() {
        "surf_start" | "map_start" => return Some(ZoneKind::Start),
        "surf_finish" | "surf_end" | "map_end" => return Some(ZoneKind::Finish),
        _ => {}
    }
    if let Some(index) = name
        .strip_prefix("surf_checkpoint_")
        .and_then(|value| value.parse().ok())
    {
        return Some(ZoneKind::Checkpoint(index));
    }
    if class == "trigger_hurt" && entity.get("damage")?.parse::<f32>().ok()? >= 100.0 {
        return Some(ZoneKind::Reset);
    }
    if class != "trigger_teleport"
        || entity.get("landmark").is_some_and(|name| !name.is_empty())
        || entity
            .get("filtername")
            .is_some_and(|name| !name.is_empty())
    {
        return None;
    }
    let target = entity.get("target")?;
    let destination = entities.iter().find(|entity| {
        entity
            .get("targetname")
            .is_some_and(|name| name.eq_ignore_ascii_case(target))
    })?;
    let flags = entity
        .get("spawnflags")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or_default();
    Some(ZoneKind::Teleport {
        position: destination.origin()?,
        yaw: (flags & 0x20 == 0).then(|| destination.yaw()),
    })
}

fn read_entities(mut text: &str) -> Result<Vec<Entity<'_>>, String> {
    let mut entities = Vec::new();
    while let Some(token) = next_token(&mut text)? {
        if token != "{" {
            return Err("Expected a new entity in the BSP entity lump".into());
        }
        let mut entity = HashMap::new();
        loop {
            let key = next_token(&mut text)?.ok_or("BSP entity ended before its closing brace")?;
            if key == "}" {
                break;
            }
            let value = next_token(&mut text)?.ok_or("Missing BSP entity value")?;
            entity.insert(key.to_ascii_lowercase(), value);
        }
        entities.push(Entity(entity));
    }
    Ok(entities)
}

fn next_token<'a>(text: &mut &'a str) -> Result<Option<&'a str>, String> {
    *text = text.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '\0');
    let Some(first) = text.as_bytes().first().copied() else {
        return Ok(None);
    };
    match first {
        b'{' | b'}' => {
            let token = &text[..1];
            *text = &text[1..];
            Ok(Some(token))
        }
        b'"' => {
            let mut escaped = false;
            for (index, byte) in text.as_bytes().iter().copied().enumerate().skip(1) {
                if byte == b'"' && !escaped {
                    let token = &text[1..index];
                    *text = &text[index + 1..];
                    return Ok(Some(token));
                }
                escaped = byte == b'\\' && !escaped;
            }
            Err("Unterminated quoted value in BSP entity lump".into())
        }
        _ => Err("Unexpected token in BSP entity lump".into()),
    }
}

fn parse_vector(value: &str) -> Option<Vec3> {
    let mut parts = value.split_ascii_whitespace();
    let vector = Vec3::new(
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    (vector.is_finite() && parts.next().is_none()).then_some(vector)
}

fn source_axes(vector: Vec3) -> Vec3 {
    Vec3::new(vector.x, vector.z, -vector.y)
}

fn lookup<'a, T>(data: &'a [T], index: usize, label: &str) -> Result<&'a T, String> {
    data.get(index)
        .ok_or_else(|| format!("Invalid BSP {label} index {index}"))
}

fn records<'a, T>(
    data: &'a [T],
    first: usize,
    count: usize,
    label: &str,
) -> Result<&'a [T], String> {
    data.get(first..first.saturating_add(count))
        .ok_or_else(|| format!("Invalid BSP {label} range"))
}

fn u16_at(bytes: &[u8], offset: usize) -> usize {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize
}

fn i16_at(bytes: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn i32_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn f32_at(bytes: &[u8], offset: usize) -> f32 {
    f32::from_bits(u32_at(bytes, offset))
}

fn vector_at(bytes: &[u8], offset: usize) -> Vec3 {
    Vec3::new(
        f32_at(bytes, offset),
        f32_at(bytes, offset + 4),
        f32_at(bytes, offset + 8),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_brushes_faces_and_teleports_share_the_same_axes() {
        let course = parse(&fixture(), "fixture", None).unwrap();
        assert_eq!(course.spawn, Vec3::new(15.0, 35.0, -25.0));
        assert_eq!(course.spawn_yaw, 0.0);
        assert_eq!(course.brushes.len(), 1);
        assert_eq!(course.brushes[0].min, Vec3::new(0.0, 0.0, -20.0));
        assert_eq!(course.brushes[0].max, Vec3::new(10.0, 30.0, 0.0));

        let [a, b, c] = course.surfaces[0].vertices.as_slice() else {
            panic!("Expected a triangle")
        };
        assert!((*b - *a).cross(*c - *a).dot(Vec3::Y) > 0.0);

        assert_eq!(course.zones.len(), 1);
        let zone = &course.zones[0];
        assert_eq!(zone.brush.min, Vec3::new(4.0, 6.0, -25.0));
        let ZoneKind::Teleport { position, yaw } = zone.kind else {
            panic!("Expected a teleport")
        };
        assert_eq!(position, Vec3::new(100.0, 300.0, -200.0));
        assert_eq!(yaw, Some(-std::f32::consts::FRAC_PI_2));
    }

    #[test]
    fn rejects_a_lump_outside_the_file() {
        let mut bytes = fixture();
        let entry = 8 + PLANES * 16;
        put(&mut bytes, entry, u32::MAX.to_le_bytes());
        let error = parse(&bytes, "truncated", None).err().unwrap();
        assert!(error.contains("invalid range"));
    }

    #[test]
    fn reads_a_valve_lzma_lump() {
        let contents = b"{\n\"classname\" \"worldspawn\"\n}\n\0";
        let mut stream = Vec::new();
        lzma_rs::lzma_compress(&mut Cursor::new(contents), &mut stream).unwrap();
        let mut lump = b"LZMA".to_vec();
        lump.extend((contents.len() as u32).to_le_bytes());
        lump.extend(((stream.len() - 13) as u32).to_le_bytes());
        lump.extend(&stream[..5]);
        lump.extend(&stream[13..]);

        let mut bytes = header();
        add_lump(&mut bytes, ENTITIES, 0, &lump);
        put(
            &mut bytes,
            8 + ENTITIES * 16 + 12,
            (contents.len() as u32).to_le_bytes(),
        );
        assert_eq!(
            &*BspFile::new(&bytes).unwrap().lump(ENTITIES).unwrap(),
            contents
        );
    }

    fn header() -> Vec<u8> {
        let mut bytes = vec![0; HEADER_SIZE];
        put(&mut bytes, 0, *b"VBSP");
        put(&mut bytes, 4, 20i32.to_le_bytes());
        bytes
    }

    fn fixture() -> Vec<u8> {
        let mut bytes = header();
        add_lump(&mut bytes, ENTITIES, 0, br#"
            { "classname" "worldspawn" }
            { "classname" "info_player_counterterrorist" "origin" "15 25 35" "angles" "0 90 0" }
            { "classname" "trigger_teleport" "model" "*1" "origin" "4 5 6" "target" "Landing" }
            { "classname" "info_teleport_destination" "targetname" "landing" "origin" "100 200 300" }
        "#);
        let mut models = [0; 96];
        put(&mut models, 44, 1i32.to_le_bytes());
        put(&mut models, 48 + 36, (-2i32).to_le_bytes());
        add_lump(&mut bytes, MODELS, 0, &models);

        let mut node = [0; 32];
        put(&mut node, 4, (-1i32).to_le_bytes());
        put(&mut node, 8, (-1i32).to_le_bytes());
        add_lump(&mut bytes, NODES, 0, &node);
        let mut leaves = [0; 64];
        put(&mut leaves, 26, 1u16.to_le_bytes());
        put(&mut leaves, 32 + 24, 1u16.to_le_bytes());
        put(&mut leaves, 32 + 26, 1u16.to_le_bytes());
        add_lump(&mut bytes, LEAVES, 1, &leaves);
        add_lump(&mut bytes, LEAF_BRUSHES, 0, &[0, 0, 1, 0]);

        let mut planes = [0; 120];
        for (index, (axis, distance)) in [
            (Vec3::X, 10.0f32),
            (Vec3::NEG_X, 0.0),
            (Vec3::Y, 20.0),
            (Vec3::NEG_Y, 0.0),
            (Vec3::Z, 30.0),
            (Vec3::NEG_Z, 0.0),
        ]
        .into_iter()
        .enumerate()
        {
            put_vector(&mut planes, 20 * index, axis);
            put(&mut planes, 20 * index + 12, distance.to_le_bytes());
        }
        add_lump(&mut bytes, PLANES, 0, &planes);
        let mut sides = [0; 48];
        for index in 0..6 {
            put(&mut sides, index * 8, (index as u16).to_le_bytes());
        }
        add_lump(&mut bytes, BRUSH_SIDES, 0, &sides);
        let mut brushes = [0; 24];
        put(&mut brushes, 4, 6u32.to_le_bytes());
        put(&mut brushes, 8, 1u32.to_le_bytes());
        put(&mut brushes, 16, 6u32.to_le_bytes());
        add_lump(&mut bytes, BRUSHES, 0, &brushes);

        let mut vertices = [0; 36];
        for (index, vertex) in [
            Vec3::new(0.0, 0.0, 30.0),
            Vec3::new(10.0, 0.0, 30.0),
            Vec3::new(0.0, 20.0, 30.0),
        ]
        .into_iter()
        .enumerate()
        {
            put_vector(&mut vertices, index * 12, vertex);
        }
        add_lump(&mut bytes, VERTICES, 0, &vertices);
        // Face uses the reverse end of edge 2 and starts with inward winding.
        add_lump(
            &mut bytes,
            EDGES,
            0,
            &[0, 0, 0, 0, 0, 0, 2, 0, 1, 0, 2, 0, 1, 0, 0, 0],
        );
        let edges: Vec<u8> = [1i32, -2, 3]
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect();
        add_lump(&mut bytes, SURFEDGES, 0, &edges);
        let mut face = [0; 56];
        put(&mut face, 0, 4u16.to_le_bytes());
        put(&mut face, 8, 3u16.to_le_bytes());
        put(&mut face, 12, (-1i16).to_le_bytes());
        add_lump(&mut bytes, FACES, 1, &face);
        add_lump(&mut bytes, TEXINFO, 0, &[0; 72]);
        bytes
    }

    fn add_lump(bytes: &mut Vec<u8>, index: usize, version: u32, contents: &[u8]) {
        let entry = 8 + index * 16;
        let offset = bytes.len() as u32;
        put(bytes, entry, offset.to_le_bytes());
        put(bytes, entry + 4, (contents.len() as u32).to_le_bytes());
        put(bytes, entry + 8, version.to_le_bytes());
        bytes.extend(contents);
    }

    fn put_vector(bytes: &mut [u8], offset: usize, vector: Vec3) {
        for (index, value) in vector.to_array().into_iter().enumerate() {
            put(bytes, offset + index * 4, value.to_le_bytes());
        }
    }

    fn put<const N: usize>(bytes: &mut [u8], offset: usize, value: [u8; N]) {
        bytes[offset..offset + N].copy_from_slice(&value);
    }
}
