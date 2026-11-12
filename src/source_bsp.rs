//! Reads the static geometry and basic entities of little-endian Source 1 maps.
//!
//! Lump layouts follow Valve's Source SDK `bspfile.h` and `bspflags.h`.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use bevy_math::{DVec3, Quat, Vec3};
use flate2::read::MultiGzDecoder;
use lzma_rs::decompress::{Options, UnpackedSize};
use sha2::{Digest, Sha256};

use crate::catalog::{CatalogEntry, MapMode};
use crate::course::{Brush, Course, Plane, Surface, SurfaceTexture, Zone};
use crate::source_material::Materials;

#[path = "source_bsp_entities.rs"]
mod gameplay;
#[path = "source_bsp_timer.rs"]
mod timer;
pub(crate) use timer::SourceTimer;

#[path = "source_lightmap.rs"]
pub(crate) mod lightmap;
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
    load_file(path, None, true)
}

pub fn load_ranked(path: &Path, entry: &CatalogEntry) -> Result<Course, String> {
    load_file(path, Some(entry), true)
}

pub(crate) fn load_ranked_collision(path: &Path, entry: &CatalogEntry) -> Result<Course, String> {
    load_file(path, Some(entry), false)
}

pub fn ranked_files_match(path: &Path, entry: &CatalogEntry) -> bool {
    let Some(expected) = &entry.source_bsp else {
        return false;
    };
    let Ok(bytes) = read_bsp_bytes(path) else {
        return false;
    };
    timer::Sidecar::read(path, &bytes)
        .ok()
        .flatten()
        .is_some_and(|timer| timer.verify_ranked(expected, entry.mode).is_ok())
}

pub fn map_name(path: &Path) -> Option<&str> {
    let uncompressed = if is_gzip(path) {
        Path::new(path.file_stem()?)
    } else {
        path
    };
    uncompressed
        .extension()
        .filter(|ext| ext.eq_ignore_ascii_case("bsp"))?;
    uncompressed.file_stem()?.to_str()
}

pub fn sidecar_path(path: &Path, extension: &str) -> PathBuf {
    if is_gzip(path) {
        path.with_extension("").with_extension(extension)
    } else {
        path.with_extension(extension)
    }
}

#[derive(Default, serde::Deserialize)]
pub struct MapMetadata {
    pub mode: Option<MapMode>,
    pub tier: Option<u8>,
    #[serde(skip)]
    pub timed: bool,
    #[serde(skip)]
    pub(crate) timer_bsp_sha256: Option<String>,
}

pub fn map_metadata(path: &Path) -> MapMetadata {
    let read = |extension| {
        let file = File::open(sidecar_path(path, extension)).ok()?;
        serde_json::from_reader::<_, MapMetadata>(BufReader::new(file.take(2 * 1024 * 1024))).ok()
    };
    let metadata = read("metadata.json").unwrap_or_default();
    let timer = timer::Sidecar::inspect(path);
    MapMetadata {
        mode: timer.as_ref().map(|(mode, _, _)| *mode).or(metadata.mode),
        tier: metadata
            .tier
            .or_else(|| timer.as_ref().and_then(|(_, tier, _)| *tier))
            .filter(|tier| *tier > 0),
        timed: timer.is_some(),
        timer_bsp_sha256: timer.map(|(_, _, sha256)| sha256),
    }
}

#[derive(PartialEq, serde::Deserialize, serde::Serialize)]
struct BspVerification<'a> {
    size: u64,
    modified_ns: u128,
    sha256: &'a str,
}

pub(crate) fn matches_timer_bsp(path: &Path, sha256: &str) -> bool {
    let verify = || -> Option<bool> {
        let file = File::open(path).ok()?;
        let stamp = |file: &File| {
            let info = file.metadata().ok()?;
            Some(BspVerification {
                size: info.len(),
                modified_ns: info
                    .modified()
                    .ok()?
                    .duration_since(UNIX_EPOCH)
                    .ok()?
                    .as_nanos(),
                sha256,
            })
        };
        let before = stamp(&file)?;
        let mut cache_path = path.as_os_str().to_owned();
        cache_path.push(".timer-check.json");
        if let Ok(cache) = File::open(&cache_path) {
            let mut text = String::new();
            if cache.take(512).read_to_string(&mut text).is_ok()
                && serde_json::from_str::<BspVerification>(&text).is_ok_and(|check| check == before)
            {
                return Some(true);
            }
        }
        let mut digest = Sha256::new();
        let mut bytes = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        let mut reader: Box<dyn Read> = if is_gzip(path) {
            Box::new(MultiGzDecoder::new(&file))
        } else {
            Box::new(&file)
        };
        loop {
            let count = reader.read(&mut buffer).ok()?;
            if count == 0 {
                break;
            }
            bytes += count as u64;
            if bytes > MAX_FILE_SIZE {
                return Some(false);
            }
            digest.update(&buffer[..count]);
        }
        let matched = format!("{:x}", digest.finalize()).eq_ignore_ascii_case(sha256)
            && stamp(&file)? == before;
        if matched && let Ok(cache) = serde_json::to_vec(&before) {
            let _ = std::fs::write(cache_path, cache);
        }
        Some(matched)
    };
    verify().unwrap_or(false)
}

fn is_gzip(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"))
}

pub(crate) fn read_bsp_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| {
            if is_gzip(path) {
                MultiGzDecoder::new(file)
                    .take(MAX_FILE_SIZE + 1)
                    .read_to_end(&mut bytes)
            } else {
                file.take(MAX_FILE_SIZE + 1).read_to_end(&mut bytes)
            }
        })
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_FILE_SIZE {
        return Err(format!("{} exceeds the 512 MiB BSP limit", path.display()));
    }
    Ok(bytes)
}

fn load_file(
    path: &Path,
    ranked: Option<&CatalogEntry>,
    load_visuals: bool,
) -> Result<Course, String> {
    let bytes = read_bsp_bytes(path)?;
    let name = map_name(path).ok_or("Expected a .bsp or .bsp.gz map")?;
    let result = (|| {
        let timer = timer::Sidecar::read(path, &bytes)?;
        let revision = if let Some(entry) = ranked {
            if !entry.id.eq_ignore_ascii_case(name) {
                return Err("Installed BSP filename differs from the ranked map name".into());
            }
            let source = entry.source_bsp.as_ref().ok_or("No ranked BSP revision")?;
            timer
                .as_ref()
                .ok_or("Ranked BSP requires its pinned timer file")?
                .verify_ranked(source, entry.mode)?;
            Some(source.board_revision())
        } else {
            None
        };
        let mut course = parse(
            &bytes,
            ranked.map_or(name, |entry| &entry.id),
            Some(path),
            timer,
            load_visuals,
        )?;
        course.catalog_revision = revision;
        if course.map_triggers.source_timer.is_none() {
            course.map_triggers.source_mode = map_metadata(path).mode;
        }
        Ok(course)
    })();
    result.map_err(|error: String| format!("{}: {error}", path.display()))
}

fn parse(
    bytes: &[u8],
    name: &str,
    map_path: Option<&Path>,
    timer: Option<timer::Sidecar>,
    load_visuals: bool,
) -> Result<Course, String> {
    let file = BspFile::new(bytes)?;
    let geometry = Geometry::read(&file)?;
    let pak = load_visuals
        .then(|| file.lump(PAKFILE).unwrap_or_default())
        .unwrap_or_default();
    let mut materials = Materials::new(&pak, map_path.filter(|_| load_visuals));
    let lighting = load_visuals
        .then(|| file.lump(geometry.lighting).unwrap_or_default())
        .unwrap_or_default();
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
    let mut surfaces = if load_visuals {
        geometry.model_surfaces(world, Placement::IDENTITY, &mut materials, &mut lightmaps)?
    } else {
        Vec::new()
    };
    let mut zones = Vec::new();
    let mut conveyors = Vec::new();
    for (index, entity) in entities.iter().enumerate() {
        let Some(model_index) = entity.model_index().filter(|&index| index != 0) else {
            continue;
        };
        let model = lookup(&geometry.models, model_index, "brush model")?;
        if entity.get("startdisabled") == Some("1") {
            continue;
        }

        let placement = entity.placement();
        if let Some(kind) = gameplay::trigger(entity, &entities, index, timer.is_some()) {
            zones.extend(
                geometry
                    .model_brushes(model, placement, false)?
                    .into_iter()
                    .map(|brush| Zone {
                        brush,
                        kind: kind.clone(),
                    }),
            );
        } else if let Some(surface) = gameplay::brush_surface_kind(
            entity,
            timer.as_ref().is_some_and(timer::Sidecar::is_bhop),
        ) {
            if load_visuals {
                surfaces.extend(geometry.model_surfaces(
                    model,
                    placement,
                    &mut materials,
                    &mut lightmaps,
                )?);
            }
            if surface.solid {
                let first = brushes.len();
                brushes.extend(geometry.model_brushes(model, placement, true)?);
                conveyors.extend(gameplay::conveyor(entity, first..brushes.len()));
            }
        }
    }

    if brushes.is_empty() || load_visuals && surfaces.is_empty() {
        return Err("The map has no supported static brush geometry".into());
    }
    let kill_y = brushes
        .iter()
        .map(|brush| brush.min.y)
        .fold(spawn.y, f32::min)
        - 2048.0;
    let mut textures = materials.into_textures();
    lightmaps.append(&mut textures, &mut surfaces);
    let mut course = Course {
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
        map_triggers: crate::course::MapTriggers {
            conveyors,
            source_world_limit: (file.version() == 25).then_some(131_072.0),
            ..Default::default()
        },
    };
    if let Some(timer) = timer {
        timer.apply(&mut course, &geometry, &entities)?;
    }
    Ok(course)
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
        if !matches!(version, 19..=21 | 25) {
            return Err(format!(
                "Unsupported VBSP version {version}. Expected 19, 20, 21 or Strata 25"
            ));
        }
        Ok(Self { bytes })
    }

    fn entry(&self, index: usize) -> &[u8] {
        let start = 8 + index * 16;
        &self.bytes[start..start + 16]
    }

    fn lump_version(&self, index: usize) -> i32 {
        i32_at(self.entry(index), 8)
    }

    fn version(&self) -> i32 {
        i32_at(self.bytes, 4)
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
    texture: i32,
    displacement: i32,
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
        let leaves = match file.lump_version(LEAVES) {
            0 => file.records::<56, _>(LEAVES, leaf)?,
            1 => file.records::<32, _>(LEAVES, leaf)?,
            2 => file.records::<56, _>(LEAVES, |row| {
                [u32_at(row, 44) as usize, u32_at(row, 48) as usize]
            })?,
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
        let children = |row: &[u8]| [i32_at(row, 4), i32_at(row, 8)];
        let nodes = match file.lump_version(NODES) {
            0 => file.records::<32, _>(NODES, children)?,
            1 => file.records::<48, _>(NODES, children)?,
            version => return Err(format!("Unsupported Source node version {version}")),
        };
        let leaf_brushes = match file.lump_version(LEAF_BRUSHES) {
            0 => file.records::<2, _>(LEAF_BRUSHES, |row| u16_at(row, 0))?,
            1 => file.records::<4, _>(LEAF_BRUSHES, |row| u32_at(row, 0) as usize)?,
            version => return Err(format!("Unsupported Source leaf brush version {version}")),
        };
        let edges = match file.lump_version(EDGES) {
            0 => file.records::<4, _>(EDGES, |row| [u16_at(row, 0), u16_at(row, 2)])?,
            1 => file.records::<8, _>(EDGES, |row| {
                [u32_at(row, 0) as usize, u32_at(row, 4) as usize]
            })?,
            version => return Err(format!("Unsupported Source edge version {version}")),
        };
        let sides = match file.lump_version(BRUSH_SIDES) {
            0 => file.records::<8, _>(BRUSH_SIDES, |row| u16_at(row, 0))?,
            1 => file.records::<16, _>(BRUSH_SIDES, |row| u32_at(row, 0) as usize)?,
            version => return Err(format!("Unsupported Source brush side version {version}")),
        };
        let lightmap = |row: &[u8], offset| FaceLightmap {
            offset: i32_at(row, offset),
            mins: [i32_at(row, offset + 8), i32_at(row, offset + 12)],
            extents: [i32_at(row, offset + 16), i32_at(row, offset + 20)],
        };
        let faces = match file.lump_version(faces_lump) {
            0 | 1 => file.records::<56, _>(faces_lump, |row| Face {
                plane: u16_at(row, 0),
                reversed: row[2] != 0,
                first_edge: u32_at(row, 4) as usize,
                edge_count: u16_at(row, 8),
                texture: i32::from(i16_at(row, 10)),
                displacement: i32::from(i16_at(row, 12)),
                lightmap: lightmap(row, 20),
            })?,
            2 => file.records::<72, _>(faces_lump, |row| Face {
                plane: u32_at(row, 0) as usize,
                reversed: row[4] != 0,
                first_edge: u32_at(row, 8) as usize,
                edge_count: u32_at(row, 12) as usize,
                texture: i32_at(row, 16),
                displacement: i32_at(row, 20),
                lightmap: lightmap(row, 32),
            })?,
            version => return Err(format!("Unsupported Source face version {version}")),
        };
        Ok(Self {
            planes: file.records::<20, _>(PLANES, |row| Plane {
                normal: source_axes(vector_at(row, 0)),
                distance: f32_at(row, 12),
            })?,
            nodes,
            leaves,
            leaf_brushes,
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
            sides,
            vertices: file.records::<12, _>(VERTICES, |row| source_axes(vector_at(row, 0)))?,
            edges,
            face_edges: file.records::<4, _>(SURFEDGES, |row| i32_at(row, 0))?,
            faces,
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
        let (min, max) =
            bounds_from_plane_intersections(&planes).ok_or("BSP brush is missing valid bounds")?;
        return Ok(Brush {
            planes: planes.into(),
            min,
            max,
        });
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

pub(crate) fn convex_brush(planes: &[Plane]) -> Result<Brush, String> {
    brush_from_planes(planes.iter().map(Ok), Placement::IDENTITY)
}

fn bounds_from_plane_intersections(planes: &[Plane]) -> Option<(Vec3, Vec3)> {
    if !(4..=64).contains(&planes.len()) {
        return None;
    }
    let planes: Vec<_> = planes
        .iter()
        .map(|plane| (plane.normal.as_dvec3(), f64::from(plane.distance)))
        .collect();
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    for (i, &(a, da)) in planes.iter().enumerate() {
        for (j, &(b, db)) in planes.iter().enumerate().skip(i + 1) {
            for &(c, dc) in &planes[j + 1..] {
                let bc = b.cross(c);
                let determinant = a.dot(bc);
                if determinant.abs() < 1e-8 {
                    continue;
                }
                let point = (bc * da + c.cross(a) * db + a.cross(b) * dc) / determinant;
                if planes
                    .iter()
                    .all(|&(normal, distance)| normal.dot(point) <= distance + 0.015625)
                {
                    min = min.min(point);
                    max = max.max(point);
                }
            }
        }
    }
    let min = min.as_vec3();
    let max = max.as_vec3();
    (min.is_finite() && max.is_finite() && min.cmple(max).all())
        .then_some((min - Vec3::splat(0.03125), max + Vec3::splat(0.03125)))
}

pub(crate) struct Entity<'a> {
    values: HashMap<String, &'a str>,
    outputs: Vec<(&'a str, &'a str)>,
}

impl<'a> Entity<'a> {
    pub(crate) fn get(&self, key: &str) -> Option<&'a str> {
        self.values.get(key).copied()
    }

    pub(crate) fn origin(&self) -> Option<Vec3> {
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

    pub(crate) fn yaw(&self) -> f32 {
        (self.angles().y.rem_euclid(360.0) - 90.0).to_radians()
    }

    pub(crate) fn model_index(&self) -> Option<usize> {
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

pub(crate) fn read_entities(mut text: &str) -> Result<Vec<Entity<'_>>, String> {
    let mut entities = Vec::new();
    while let Some(token) = next_token(&mut text)? {
        if token != "{" {
            return Err("Expected a new entity in the BSP entity lump".into());
        }
        let mut entity = HashMap::new();
        let mut outputs = Vec::new();
        loop {
            let key = next_token(&mut text)?.ok_or("BSP entity ended before its closing brace")?;
            if key == "}" {
                break;
            }
            let value = next_token(&mut text)?.ok_or("Missing BSP entity value")?;
            let lower = key.to_ascii_lowercase();
            if lower.starts_with("on") {
                outputs.push((key, value));
            }
            entity.insert(lower, value);
        }
        entities.push(Entity {
            values: entity,
            outputs,
        });
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

pub(crate) fn source_axes(vector: Vec3) -> Vec3 {
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
    use crate::course::ZoneKind;

    #[test]
    fn derives_bounds_when_a_brush_has_no_axial_bevels() {
        let planes = [
            Plane {
                normal: Vec3::NEG_X,
                distance: 0.0,
            },
            Plane {
                normal: Vec3::NEG_Y,
                distance: 0.0,
            },
            Plane {
                normal: Vec3::NEG_Z,
                distance: 0.0,
            },
            Plane {
                normal: Vec3::ONE.normalize(),
                distance: 1.0 / 3.0_f32.sqrt(),
            },
        ];
        let brush = brush_from_planes(planes.iter().map(Ok), Placement::IDENTITY).unwrap();
        assert!(brush.min.distance(Vec3::splat(-0.03125)) < 0.00001);
        assert!(brush.max.distance(Vec3::splat(1.03125)) < 0.00001);
    }

    #[test]
    fn source_brushes_faces_and_teleports_share_the_same_axes() {
        let course = parse(&fixture(), "fixture", None, None, true).unwrap();
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
        let error = parse(&bytes, "truncated", None, None, true).err().unwrap();
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
