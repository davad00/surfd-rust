//! Quake and GoldSrc BSP geometry, with local QuakeWorld KTX race routes.

use std::collections::HashMap;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

use bevy_math::Vec3;
use sha2::{Digest, Sha256};

use crate::catalog::MapMode;
use crate::course::{
    BonusRoute, Brush, Course, CourseTexture, MapTriggers, Plane, PracticeSpawn, Surface,
    SurfaceTexture, TextureAlpha, Zone, ZoneKind,
};
use crate::movement::source_yaw_to_native;
use crate::source_bsp::lightmap::{FaceLightmap, Lightmaps};
use crate::source_bsp::{map_name, read_bsp_bytes, sidecar_path, source_axes};

const LUMP_COUNT: usize = 15;
const HEADER_BYTES: usize = 4 + LUMP_COUNT * 8;
const MAX_ROUTE_BYTES: u64 = 256 * 1024;
const MAX_COLLISION_CELLS: usize = 131_072;
const EPSILON: f32 = 0.001;
const QUAKE_ORIGIN_ABOVE_FEET: f32 = 24.0;

pub fn load(path: &Path) -> Result<Course, String> {
    let load = || {
        let bytes = read_map(path)?;
        let name = map_name(path).ok_or("Invalid Quake map filename")?;
        let bsp = LegacyBsp::parse(&bytes, 29)?;
        let palette = quake_palette(path);
        let textures = quake_textures(bsp.texture_lump(), palette.as_deref())?;
        let mut course = bsp.build(name, textures)?;
        if let Some(text) = read_optional(&sidecar_path(path, "route"), MAX_ROUTE_BYTES)? {
            let routes = RouteFile::parse(&String::from_utf8_lossy(&text))?;
            routes.apply(&bsp, &mut course, Sha256::digest(&text).into())?;
        }
        course.map_triggers.source_mode = Some(map_mode(name));
        Ok(course)
    };
    load().map_err(|error: String| format!("{}: {error}", path.display()))
}

pub fn map_mode(name: &str) -> MapMode {
    let name = name.to_ascii_lowercase();
    if name.contains("surf") || name.starts_with("ramp") {
        MapMode::Surf
    } else {
        MapMode::Bhop
    }
}

pub(crate) fn read_map(path: &Path) -> Result<Vec<u8>, String> {
    read_bsp_bytes(path)
}

pub(crate) fn native(value: Vec3) -> Vec3 {
    source_axes(value)
}

pub(crate) struct LegacyBsp<'a> {
    version: u32,
    entities: Vec<LegacyEntity>,
    texture_lump: &'a [u8],
    lighting: &'a [u8],
    planes: Vec<Plane>,
    vertices: Vec<Vec3>,
    nodes: Vec<Node>,
    texinfo: Vec<TexInfo>,
    faces: Vec<Face>,
    contents: Vec<i32>,
    edges: Vec<[usize; 2]>,
    face_edges: Vec<i32>,
    models: Vec<Model>,
}

struct Node {
    plane: usize,
    children: [i32; 2],
}

struct TexInfo {
    axes: [(Vec3, f32); 2],
    texture: i32,
    special: bool,
}

struct Face {
    plane: usize,
    back: bool,
    first_edge: usize,
    edge_count: usize,
    texinfo: usize,
    light_offset: i32,
}

struct Model {
    min: Vec3,
    max: Vec3,
    root: i32,
    first_face: usize,
    face_count: usize,
}

pub(crate) struct LegacyEntity(HashMap<String, String>);

impl LegacyEntity {
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub(crate) fn origin(&self) -> Option<Vec3> {
        Some(native(vector(
            self.get("origin")?.split_ascii_whitespace(),
        )?))
    }

    pub(crate) fn model_index(&self) -> Option<usize> {
        self.get("model")?.strip_prefix('*')?.parse().ok()
    }

    pub(crate) fn yaw(&self) -> f32 {
        source_yaw_to_native(f64::from(self.angles().y))
    }

    fn angles(&self) -> Vec3 {
        if let Some(angles) = self
            .get("angles")
            .and_then(|text| vector(text.split_ascii_whitespace()))
        {
            return angles;
        }
        match self.get("angle").and_then(number).unwrap_or_default() {
            -1.0 => Vec3::new(-90.0, 0.0, 0.0),
            -2.0 => Vec3::new(90.0, 0.0, 0.0),
            yaw => Vec3::new(0.0, yaw, 0.0),
        }
    }
}

impl<'a> LegacyBsp<'a> {
    pub(crate) fn parse(bytes: &'a [u8], version: u32) -> Result<Self, String> {
        if bytes.len() < HEADER_BYTES || !matches!(version, 29 | 30) || u32_at(bytes, 0) != version
        {
            return Err(format!("Expected a BSP{version} map"));
        }
        let mut lumps = [&[][..]; LUMP_COUNT];
        for (index, lump) in lumps.iter_mut().enumerate() {
            let offset = u32_at(bytes, 4 + index * 8) as usize;
            let len = u32_at(bytes, 8 + index * 8) as usize;
            if len == 0 {
                continue;
            }
            *lump = bytes
                .get(offset..offset.saturating_add(len))
                .filter(|_| offset >= HEADER_BYTES)
                .ok_or_else(|| format!("Invalid BSP lump {index} range"))?;
        }
        let planes = records(lumps[1], 20, "plane", |r| {
            let normal = native(vec_at(r, 0));
            let length = normal.length();
            let distance = f32_at(r, 12);
            if !length.is_finite() || length < 0.5 || !distance.is_finite() {
                return Err("Invalid BSP plane".into());
            }
            Ok(Plane {
                normal: normal / length,
                distance: distance / length,
            })
        })?;
        Ok(Self {
            version,
            entities: parse_entities(&String::from_utf8_lossy(lumps[0]))?,
            texture_lump: lumps[2],
            lighting: lumps[8],
            planes,
            vertices: records(lumps[3], 12, "vertex", |r| {
                let point = native(vec_at(r, 0));
                point
                    .is_finite()
                    .then_some(point)
                    .ok_or("Invalid BSP vertex".into())
            })?,
            nodes: records(lumps[5], 24, "node", |r| {
                Ok(Node {
                    plane: u32_at(r, 0) as usize,
                    children: [i32::from(i16_at(r, 4)), i32::from(i16_at(r, 6))],
                })
            })?,
            texinfo: records(lumps[6], 40, "texture info", |r| {
                let axes =
                    [0, 16].map(|offset| (native(vec_at(r, offset)), f32_at(r, offset + 12)));
                if !axes
                    .iter()
                    .all(|(axis, offset)| axis.is_finite() && offset.is_finite())
                {
                    return Err("Invalid BSP texture axes".into());
                }
                Ok(TexInfo {
                    axes,
                    texture: i32_at(r, 32),
                    special: i32_at(r, 36) & 1 != 0,
                })
            })?,
            faces: records(lumps[7], 20, "face", |r| {
                Ok(Face {
                    plane: u16_at(r, 0),
                    back: u16_at(r, 2) != 0,
                    first_edge: u32_at(r, 4) as usize,
                    edge_count: u16_at(r, 8),
                    texinfo: u16_at(r, 10),
                    light_offset: i32_at(r, 16),
                })
            })?,
            contents: records(lumps[10], 28, "leaf", |r| Ok(i32_at(r, 0)))?,
            edges: records(lumps[12], 4, "edge", |r| Ok([u16_at(r, 0), u16_at(r, 2)]))?,
            face_edges: records(lumps[13], 4, "surface edge", |r| Ok(i32_at(r, 0)))?,
            models: records(lumps[14], 64, "model", |r| {
                let a = native(vec_at(r, 0));
                let b = native(vec_at(r, 12));
                if !a.is_finite()
                    || !b.is_finite()
                    || a.abs().max(b.abs()).max_element() > 1_000_000.0
                {
                    return Err("Invalid BSP model bounds".into());
                }
                Ok(Model {
                    min: a.min(b),
                    max: a.max(b),
                    root: i32_at(r, 36),
                    first_face: u32_at(r, 56) as usize,
                    face_count: u32_at(r, 60) as usize,
                })
            })?,
        })
    }

    pub(crate) fn texture_lump(&self) -> &'a [u8] {
        self.texture_lump
    }

    pub(crate) fn entities(&self) -> &[LegacyEntity] {
        &self.entities
    }

    pub(crate) fn model_bounds(&self, index: usize) -> Option<(Vec3, Vec3)> {
        let model = self.models.get(index)?;
        Some((model.min, model.max))
    }

    pub(crate) fn build(
        &self,
        name: &str,
        textures: Vec<Option<CourseTexture>>,
    ) -> Result<Course, String> {
        let world = self.models.first().ok_or("BSP has no world model")?;
        let mut course_textures = Vec::new();
        let texture_indices: Vec<_> = textures
            .into_iter()
            .map(|texture| {
                texture.map(|texture| {
                    let index = course_textures.len();
                    course_textures.push(texture);
                    index
                })
            })
            .collect();
        let lighting = self.light_data()?;
        let mut lightmaps = Lightmaps::new(&lighting);
        let mut surfaces = Vec::new();
        self.render_model(
            world,
            Vec3::ZERO,
            &texture_indices,
            &course_textures,
            &mut lightmaps,
            &mut surfaces,
        )?;
        let (mut brushes, mut zones) = self.collision_model(world, Vec3::ZERO, true)?;
        for entity in &self.entities {
            let Some(index) = entity.model_index().filter(|&index| index != 0) else {
                continue;
            };
            let Some(model) = self.models.get(index) else {
                return Err("Invalid BSP submodel index".into());
            };
            let position = entity.origin().unwrap_or_default();
            let class = entity.get("classname").unwrap_or_default();
            if class.starts_with("trigger_") {
                if let Some(kind) = self.trigger_kind(entity, zones.len())
                    && let Some(brush) = self.trigger_brush(entity)
                {
                    zones.push(Zone { brush, kind });
                }
            } else if class.starts_with("func_") {
                self.render_model(
                    model,
                    position,
                    &texture_indices,
                    &course_textures,
                    &mut lightmaps,
                    &mut surfaces,
                )?;
                if !matches!(
                    class,
                    "func_illusionary" | "func_wall_toggle" | "func_detail_illusionary"
                ) {
                    brushes.extend(self.collision_model(model, position, false)?.0);
                }
            }
        }
        if brushes.is_empty() || surfaces.is_empty() {
            return Err("BSP has no usable solid geometry".into());
        }
        lightmaps.append(&mut course_textures, &mut surfaces);
        let spawn = self.default_spawn(world);
        Ok(Course {
            name: name.into(),
            catalog_revision: None,
            bonus_routes: Vec::new(),
            map_triggers: MapTriggers {
                spawn_pitch: Some(spawn.pitch),
                spawn_source_yaw: spawn.source_yaw_degrees,
                ..Default::default()
            },
            spawn: spawn.position,
            spawn_yaw: spawn.yaw,
            brushes,
            surfaces,
            textures: course_textures,
            visual_meshes: Vec::new(),
            visual_visibility: None,
            practice_spawns: Vec::new(),
            sky: None,
            sky_occluders: Vec::new(),
            fog: None,
            zones,
            kill_y: world.min.y - 256.0,
        })
    }

    fn default_spawn(&self, world: &Model) -> PracticeSpawn {
        let height = self.origin_above_feet();
        let initial = [
            "info_player_start",
            "info_player_deathmatch",
            "info_player_counterterrorist",
            "info_player_terrorist",
        ]
        .iter()
        .find_map(|class| {
            self.entities.iter().find_map(|entity| {
                (entity.get("classname") == Some(*class))
                    .then_some((entity.origin()?, entity.angles()))
            })
        });
        let (origin, angles) = initial.unwrap_or(((world.min + world.max) * 0.5, Vec3::ZERO));
        spawn("Start".into(), origin, angles.x, angles.y, height)
    }

    fn origin_above_feet(&self) -> f32 {
        if self.version == 29 {
            QUAKE_ORIGIN_ABOVE_FEET
        } else {
            36.0
        }
    }

    fn trigger_kind(&self, entity: &LegacyEntity, index: usize) -> Option<ZoneKind> {
        match entity.get("classname")? {
            "trigger_teleport" => {
                let target = entity.get("target")?;
                let destination = self
                    .entities
                    .iter()
                    .find(|other| other.get("targetname") == Some(target))?;
                Some(ZoneKind::Teleport {
                    position: destination.origin()? - Vec3::Y * self.origin_above_feet(),
                    yaw: Some(destination.yaw()),
                })
            }
            "trigger_hurt" if entity.get("dmg").and_then(number).unwrap_or(5.0) >= 100.0 => {
                Some(ZoneKind::Reset)
            }
            "trigger_push" => {
                let speed = entity.get("speed").and_then(number).unwrap_or(1000.0);
                let angles = entity.angles();
                let (pitch_sin, pitch_cos) = angles.x.to_radians().sin_cos();
                let (yaw_sin, yaw_cos) = angles.y.to_radians().sin_cos();
                let direction = native(Vec3::new(
                    pitch_cos * yaw_cos,
                    pitch_cos * yaw_sin,
                    -pitch_sin,
                ));
                Some(ZoneKind::Push {
                    index,
                    velocity: direction * speed,
                })
            }
            _ => None,
        }
    }

    fn trigger_brush(&self, entity: &LegacyEntity) -> Option<Brush> {
        let (min, max) = self.model_bounds(entity.model_index()?)?;
        let origin = entity.origin().unwrap_or_default();
        Some(Brush::box_at(min + origin, max + origin))
    }

    fn light_data(&self) -> Result<Vec<u8>, String> {
        if self.lighting.len() > 32 * 1024 * 1024 {
            return Err("BSP lighting exceeds the supported size".into());
        }
        let stride = if self.version == 29 { 1 } else { 3 };
        let mut data = Vec::with_capacity(self.lighting.len() / stride * 4);
        for pixel in self.lighting.chunks_exact(stride) {
            let color = if stride == 1 {
                [pixel[0]; 3]
            } else {
                [pixel[0], pixel[1], pixel[2]]
            };
            data.extend_from_slice(&[color[0], color[1], color[2], 0]);
        }
        Ok(data)
    }

    fn render_model(
        &self,
        model: &Model,
        translation: Vec3,
        indices: &[Option<usize>],
        textures: &[CourseTexture],
        lightmaps: &mut Lightmaps<'_>,
        surfaces: &mut Vec<Surface>,
    ) -> Result<(), String> {
        let faces = range(
            &self.faces,
            model.first_face,
            model.face_count,
            "model faces",
        )?;
        for face in faces {
            if !(3..=4096).contains(&face.edge_count) {
                continue;
            }
            let info = self
                .texinfo
                .get(face.texinfo)
                .ok_or("Invalid BSP face texture info")?;
            let plane = self
                .planes
                .get(face.plane)
                .ok_or("Invalid BSP face plane")?;
            let outward = if face.back {
                -plane.normal
            } else {
                plane.normal
            };
            let face_edges = range(
                &self.face_edges,
                face.first_edge,
                face.edge_count,
                "face edges",
            )?;
            let mut vertices = Vec::with_capacity(face.edge_count);
            for &reference in face_edges {
                let edge = self
                    .edges
                    .get(reference.unsigned_abs() as usize)
                    .ok_or("Invalid BSP surface edge")?;
                let index = edge[usize::from(reference < 0)];
                vertices.push(*self.vertices.get(index).ok_or("Invalid BSP edge vertex")?);
            }
            let first = vertices[0];
            let area: Vec3 = vertices[1..]
                .windows(2)
                .map(|pair| (pair[0] - first).cross(pair[1] - first))
                .sum();
            if area.length_squared() <= f32::EPSILON {
                continue;
            }
            if area.dot(outward) < 0.0 {
                vertices.reverse();
            }
            let texture = usize::try_from(info.texture)
                .ok()
                .and_then(|index| indices.get(index).copied().flatten())
                .map(|index| {
                    let size = [textures[index].width as f32, textures[index].height as f32];
                    let uvs = vertices
                        .iter()
                        .map(|point| {
                            std::array::from_fn(|axis| {
                                (info.axes[axis].0.dot(*point) + info.axes[axis].1) / size[axis]
                            })
                        })
                        .collect();
                    SurfaceTexture { index, uvs }
                });
            let lightmap = if !info.special && face.light_offset >= 0 {
                let axes = info.axes.map(|(axis, offset)| (axis / 16.0, offset / 16.0));
                let mut min = [f32::INFINITY; 2];
                let mut max = [f32::NEG_INFINITY; 2];
                for &point in &vertices {
                    for axis in 0..2 {
                        let value = axes[axis].0.dot(point) + axes[axis].1;
                        min[axis] = min[axis].min(value);
                        max[axis] = max[axis].max(value);
                    }
                }
                let mins = min.map(|value| value.floor() as i32);
                let extents = std::array::from_fn(|axis| {
                    (max[axis].ceil() as i32).saturating_sub(mins[axis])
                });
                let offset = if self.version == 29 {
                    face.light_offset
                } else {
                    face.light_offset / 3
                };
                offset.checked_mul(4).and_then(|offset| {
                    lightmaps.surface(
                        FaceLightmap {
                            offset,
                            mins,
                            extents,
                        },
                        axes,
                        &vertices,
                    )
                })
            } else {
                None
            };
            for point in &mut vertices {
                *point += translation;
            }
            surfaces.push(Surface {
                vertices,
                color: [0.57, 0.54, 0.49],
                texture,
                lightmap,
            });
        }
        Ok(())
    }

    fn collision_model(
        &self,
        model: &Model,
        translation: Vec3,
        hazards: bool,
    ) -> Result<(Vec<Brush>, Vec<Zone>), String> {
        let mut pending = vec![(
            model.root,
            Cell::boxed(model.min - Vec3::splat(32.0), model.max + Vec3::splat(32.0)),
            0,
        )];
        let mut solids = Vec::new();
        let mut zones = Vec::new();
        let mut visited = 0;
        while let Some((index, cell, depth)) = pending.pop() {
            visited += 1;
            if visited > MAX_COLLISION_CELLS || depth > 256 {
                return Err("BSP collision tree exceeds the supported complexity".into());
            }
            if index < 0 {
                let leaf = (!index) as usize;
                let contents = *self
                    .contents
                    .get(leaf)
                    .ok_or("Invalid BSP collision leaf")?;
                if contents == -2 || contents == -6 {
                    if let Some(brush) = cell.into_brush(translation) {
                        solids.push(brush);
                    }
                } else if hazards
                    && matches!(contents, -4 | -5)
                    && let Some(brush) = cell.into_brush(translation)
                {
                    zones.push(Zone {
                        brush,
                        kind: ZoneKind::Reset,
                    });
                }
                continue;
            }
            let node = self
                .nodes
                .get(index as usize)
                .ok_or("Invalid BSP collision node")?;
            let plane = *self
                .planes
                .get(node.plane)
                .ok_or("Invalid BSP collision plane")?;
            let (min, max) = cell.interval(plane);
            if min >= -EPSILON {
                pending.push((node.children[0], cell, depth + 1));
            } else if max <= EPSILON {
                pending.push((node.children[1], cell, depth + 1));
            } else {
                for (child, halfspace) in [reversed(plane), plane].into_iter().enumerate() {
                    if let Some(part) = cell.clip(halfspace) {
                        pending.push((node.children[child], part, depth + 1));
                    }
                }
            }
        }
        Ok((solids, zones))
    }
}

fn spawn(name: String, origin: Vec3, pitch: f32, yaw: f32, height: f32) -> PracticeSpawn {
    PracticeSpawn {
        name,
        position: origin - Vec3::Y * height,
        yaw: source_yaw_to_native(f64::from(yaw)),
        pitch: pitch.to_radians(),
        source_yaw_degrees: Some(f64::from(yaw)),
    }
}

struct Cell {
    faces: Vec<Polygon>,
}

struct Polygon {
    plane: Plane,
    points: Vec<Vec3>,
}

impl Cell {
    fn boxed(min: Vec3, max: Vec3) -> Self {
        let center = (min + max) * 0.5;
        let half = (max - min) * 0.5;
        let mut faces = Vec::with_capacity(6);
        for axis in 0..3 {
            let mut normal = Vec3::ZERO;
            normal[axis] = 1.0;
            let mut u = Vec3::ZERO;
            u[(axis + 1) % 3] = half[(axis + 1) % 3];
            let mut v = Vec3::ZERO;
            v[(axis + 2) % 3] = half[(axis + 2) % 3];
            for side in [-1.0, 1.0] {
                let face_center = center + normal * (half[axis] * side);
                let normal = normal * side;
                faces.push(Polygon {
                    plane: Plane {
                        normal,
                        distance: normal.dot(face_center),
                    },
                    points: vec![
                        face_center - u - v,
                        face_center + u - v,
                        face_center + u + v,
                        face_center - u + v,
                    ],
                });
            }
        }
        Self { faces }
    }

    fn interval(&self, plane: Plane) -> (f32, f32) {
        self.faces.iter().flat_map(|face| &face.points).fold(
            (f32::INFINITY, f32::NEG_INFINITY),
            |(min, max), &point| {
                let distance = plane.normal.dot(point) - plane.distance;
                (min.min(distance), max.max(distance))
            },
        )
    }

    fn clip(&self, plane: Plane) -> Option<Self> {
        let mut faces = Vec::with_capacity(self.faces.len() + 1);
        let mut cut = Vec::new();
        for face in &self.faces {
            let mut polygon = Vec::with_capacity(face.points.len() + 1);
            let mut a = *face.points.last()?;
            let mut a_dist = plane.normal.dot(a) - plane.distance;
            for &b in &face.points {
                let b_dist = plane.normal.dot(b) - plane.distance;
                if (a_dist <= 0.0) != (b_dist <= 0.0) {
                    let point = a.lerp(b, a_dist / (a_dist - b_dist));
                    push_unique(&mut cut, point);
                    push_unique(&mut polygon, point);
                }
                if b_dist <= 0.0 {
                    push_unique(&mut polygon, b);
                    if b_dist.abs() < EPSILON {
                        push_unique(&mut cut, b);
                    }
                }
                a = b;
                a_dist = b_dist;
            }
            if polygon.len() >= 3 {
                faces.push(Polygon {
                    plane: face.plane,
                    points: polygon,
                });
            }
        }
        if cut.len() < 3 || faces.len() < 3 {
            return None;
        }
        let center = cut.iter().copied().sum::<Vec3>() / cut.len() as f32;
        let reference = if plane.normal.y.abs() < 0.9 {
            Vec3::Y
        } else {
            Vec3::X
        };
        let u = plane.normal.cross(reference).normalize();
        let v = plane.normal.cross(u);
        cut.sort_by(|a, b| {
            let angle = |point: Vec3| v.dot(point - center).atan2(u.dot(point - center));
            angle(*a).total_cmp(&angle(*b))
        });
        faces.push(Polygon { plane, points: cut });
        Some(Self { faces })
    }

    fn into_brush(self, translation: Vec3) -> Option<Brush> {
        let mut points = Vec::new();
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for &point in self.faces.iter().flat_map(|face| &face.points) {
            min = min.min(point);
            max = max.max(point);
            push_unique(&mut points, point);
        }
        if points.len() < 4 || (max - min).min_element() < EPSILON {
            return None;
        }
        let mut planes: Vec<_> = self.faces.iter().map(|face| face.plane).collect();
        // Add axial and edge bevels for the native axis-aligned player hull.
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            add_plane(
                &mut planes,
                Plane {
                    normal: axis,
                    distance: axis.dot(max),
                },
            );
            add_plane(
                &mut planes,
                Plane {
                    normal: -axis,
                    distance: -axis.dot(min),
                },
            );
        }
        for face in &self.faces {
            for (&a, &b) in face.points.iter().zip(face.points.iter().cycle().skip(1)) {
                for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                    let Some(normal) = (b - a).cross(axis).try_normalize() else {
                        continue;
                    };
                    for normal in [normal, -normal] {
                        let distance = normal.dot(a);
                        if points
                            .iter()
                            .all(|point| normal.dot(*point) <= distance + 0.01)
                        {
                            add_plane(&mut planes, Plane { normal, distance });
                        }
                    }
                }
            }
        }
        for plane in &mut planes {
            plane.distance += plane.normal.dot(translation);
        }
        Some(Brush {
            planes: planes.into(),
            min: min + translation,
            max: max + translation,
        })
    }
}

fn reversed(plane: Plane) -> Plane {
    Plane {
        normal: -plane.normal,
        distance: -plane.distance,
    }
}

fn add_plane(planes: &mut Vec<Plane>, plane: Plane) {
    if !planes.iter().any(|known| {
        known.normal.dot(plane.normal) > 0.99999 && (known.distance - plane.distance).abs() < 0.01
    }) {
        planes.push(plane);
    }
}

fn push_unique(points: &mut Vec<Vec3>, point: Vec3) {
    if !points
        .iter()
        .any(|known| known.distance_squared(point) < EPSILON * EPSILON)
    {
        points.push(point);
    }
}

fn quake_palette(path: &Path) -> Option<Vec<u8>> {
    let root = path.parent()?;
    let configured = std::env::var_os("SURF_QUAKE_PALETTE").map(std::path::PathBuf::from);
    configured
        .into_iter()
        .chain([
            root.join("quake-palette.lmp"),
            root.join("palette.lmp"),
            root.join("gfx/palette.lmp"),
            root.join("../gfx/palette.lmp"),
        ])
        .find_map(|file| {
            read_optional(&file, 768)
                .ok()
                .flatten()
                .filter(|data| data.len() == 768)
        })
}

fn quake_textures(
    data: &[u8],
    palette: Option<&[u8]>,
) -> Result<Vec<Option<CourseTexture>>, String> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    if data.len() < 4 {
        return Err("Invalid BSP miptex header".into());
    }
    let count = u32_at(data, 0) as usize;
    if count > 4096 {
        return Err("BSP has too many miptex textures".into());
    }
    let offsets = range(data, 4, count * 4, "miptex directory")?;
    let mut used = 0usize;
    offsets
        .chunks_exact(4)
        .map(|entry| {
            let offset = i32_at(entry, 0);
            if offset < 0 {
                return Ok(None);
            }
            let bytes = data
                .get(offset as usize..)
                .ok_or("Invalid BSP miptex offset")?;
            let header = bytes.get(..40).ok_or("Invalid BSP miptex record")?;
            let width = u32_at(header, 16);
            let height = u32_at(header, 20);
            let pixel_offset = u32_at(header, 24) as usize;
            if width == 0 || height == 0 || width > 4096 || height > 4096 || pixel_offset < 40 {
                return Ok(None);
            }
            let pixel_count = width as usize * height as usize;
            let pixels = range(bytes, pixel_offset, pixel_count, "miptex pixels")?;
            used = used.saturating_add(pixel_count * 4);
            if used > 256 * 1024 * 1024 {
                return Err("BSP decoded textures exceed the supported size".into());
            }
            let name = c_string(&header[..16]);
            let rgba = pixels
                .iter()
                .flat_map(|&index| {
                    let index = usize::from(index);
                    let color = palette.map_or_else(
                        || [index as u8; 3],
                        |palette| {
                            [
                                palette[index * 3],
                                palette[index * 3 + 1],
                                palette[index * 3 + 2],
                            ]
                        },
                    );
                    [color[0], color[1], color[2], 255]
                })
                .collect();
            Ok(Some(CourseTexture {
                name,
                width,
                height,
                rgba,
                bc: None,
                alpha: TextureAlpha::Opaque,
            }))
        })
        .collect()
}

struct RouteFile {
    routes: Vec<Route>,
    teleport_flags: HashMap<String, TeleportFlag>,
}

#[derive(Default)]
struct Route {
    name: String,
    nodes: Vec<RouteNode>,
}

struct RouteNode {
    center: Vec3,
    pitch: f32,
    yaw: f32,
    size: Option<Vec3>,
}

enum TeleportFlag {
    Fail,
    Finish,
}

impl RouteFile {
    fn parse(text: &str) -> Result<Self, String> {
        let mut file = Self {
            routes: Vec::new(),
            teleport_flags: HashMap::new(),
        };
        let mut active = None::<Route>;
        for (index, line) in text.lines().enumerate() {
            let mut tokens = Tokens(line);
            let mut args = Vec::new();
            while let Some(token) = tokens.next()? {
                args.push(token);
            }
            let result = match args.as_slice() {
                [] => Ok(()),
                ["race_route_add_start"] if active.is_none() => {
                    active = Some(Route::default());
                    Ok(())
                }
                ["race_route_add_end"] => {
                    let mut route = active.take().ok_or("Route ends before it starts")?;
                    if !(2..=256).contains(&route.nodes.len()) || file.routes.len() >= 64 {
                        return Err(
                            "KTX race route exceeds the supported size or lacks endpoints".into(),
                        );
                    }
                    if route.name.is_empty() {
                        route.name = format!("Route {}", file.routes.len() + 1);
                    }
                    file.routes.push(route);
                    Ok(())
                }
                ["race_add_route_node", x, y, z, pitch, yaw] => {
                    let route = active.as_mut().ok_or("KTX node outside a route")?;
                    let point =
                        vector([*x, *y, *z].into_iter()).ok_or("Invalid KTX node position")?;
                    let pitch = number(pitch).ok_or("Invalid KTX node pitch")?;
                    let yaw = number(yaw).ok_or("Invalid KTX node yaw")?;
                    route.nodes.push(RouteNode {
                        center: native(point),
                        pitch,
                        yaw,
                        size: None,
                    });
                    Ok(())
                }
                ["race_set_route_name", name, _] => {
                    active.as_mut().ok_or("KTX name outside a route")?.name = (*name).into();
                    Ok(())
                }
                ["race_set_node_size", x, y, z] => {
                    let node = active
                        .as_mut()
                        .and_then(|route| route.nodes.last_mut())
                        .ok_or("KTX size without a node")?;
                    let size = vector([*x, *y, *z].into_iter())
                        .filter(|size| size.cmpgt(Vec3::ZERO).all())
                        .ok_or("Invalid KTX node size")?;
                    node.size = Some(native(size).abs());
                    Ok(())
                }
                ["race_set_teleport_flags_by_name", target, flag] => {
                    let flag = match *flag {
                        "RACEFLAG_TOUCH_RACEFAIL" => TeleportFlag::Fail,
                        "RACEFLAG_TOUCH_RACEEND" => TeleportFlag::Finish,
                        _ => return Err("Unsupported KTX race teleport flag".into()),
                    };
                    file.teleport_flags.insert((*target).into(), flag);
                    Ok(())
                }
                [
                    "race_set_route_timeout"
                    | "race_set_route_weapon_mode"
                    | "race_set_route_falsestart_mode"
                    | "race_set_route_pacemaker_legal",
                    _,
                ] if active.is_some() => Ok(()),
                ["race_spawn_intermission", _, _, _, _, _, _] => Ok(()),
                _ => Err("Unsupported KTX route command"),
            };
            result.map_err(|error| format!("KTX route line {}: {error}", index + 1))?;
        }
        if active.is_some() || file.routes.is_empty() {
            return Err("KTX route file has no complete routes".into());
        }
        Ok(file)
    }

    fn apply(
        self,
        bsp: &LegacyBsp<'_>,
        course: &mut Course,
        route_hash: [u8; 32],
    ) -> Result<(), String> {
        let mut common = std::mem::take(&mut course.zones);
        for entity in bsp
            .entities()
            .iter()
            .filter(|entity| entity.get("classname") == Some("trigger_teleport"))
        {
            let Some(flag) = entity
                .get("target")
                .and_then(|target| self.teleport_flags.get(target))
            else {
                continue;
            };
            let Some(brush) = bsp.trigger_brush(entity) else {
                continue;
            };
            if let Some(zone) = common.iter_mut().find(|zone| {
                matches!(zone.kind, ZoneKind::Teleport { .. })
                    && zone.brush.min == brush.min
                    && zone.brush.max == brush.max
            }) {
                zone.kind = match flag {
                    TeleportFlag::Fail => ZoneKind::Reset,
                    TeleportFlag::Finish => ZoneKind::Finish,
                };
            }
        }
        let mut routes = self.routes.into_iter();
        let main = routes.next().ok_or("KTX route file has no main route")?;
        let initial = main.start();
        course.spawn = initial.position;
        course.spawn_yaw = initial.yaw;
        course.map_triggers.spawn_pitch = Some(initial.pitch);
        course.map_triggers.spawn_source_yaw = initial.source_yaw_degrees;
        course.zones = main.zones(&common);
        for (index, route) in routes.enumerate() {
            let spawn = route.start();
            let zones = route.zones(&common);
            let number = (index + 1) as u16;
            let revision = format!(
                "{:x}",
                Sha256::new()
                    .chain_update(route_hash)
                    .chain_update(number.to_le_bytes())
                    .finalize()
            );
            course.bonus_routes.push(BonusRoute {
                number,
                name: route.name,
                spawn,
                zones,
                revision,
            });
        }
        Ok(())
    }
}

impl Route {
    fn start(&self) -> PracticeSpawn {
        let start = &self.nodes[0];
        spawn(
            self.name.clone(),
            start.center,
            start.pitch,
            start.yaw,
            QUAKE_ORIGIN_ABOVE_FEET,
        )
    }

    fn zones(&self, common: &[Zone]) -> Vec<Zone> {
        let last = self.nodes.len() - 1;
        let mut zones = common.to_vec();
        for (index, node) in self.nodes.iter().enumerate() {
            let (min, max) = node.size.map_or(
                (Vec3::new(-16.0, -24.0, -16.0), Vec3::new(16.0, 32.0, 16.0)),
                |size| (-size * 0.5, size * 0.5),
            );
            let kind = if index == 0 {
                ZoneKind::Start
            } else if index == last {
                ZoneKind::Finish
            } else {
                ZoneKind::Checkpoint(index as u16)
            };
            zones.push(Zone {
                brush: Brush::box_at(node.center + min, node.center + max),
                kind,
            });
        }
        zones
    }
}

struct Tokens<'a>(&'a str);

impl<'a> Tokens<'a> {
    fn next(&mut self) -> Result<Option<&'a str>, String> {
        loop {
            self.0 = self.0.trim_start_matches(|character: char| {
                character.is_whitespace() || character == '\0'
            });
            if self.0.starts_with("//") {
                self.0 = self.0.split_once('\n').map_or("", |(_, tail)| tail);
            } else {
                break;
            }
        }
        let Some(&first) = self.0.as_bytes().first() else {
            return Ok(None);
        };
        let token = self.0;
        let (start, end, advance) = match first {
            b'{' | b'}' => (0, 1, 1),
            b'"' => {
                let end = token[1..]
                    .find('"')
                    .map(|index| index + 1)
                    .ok_or("Unterminated BSP quoted token")?;
                (1, end, end + 1)
            }
            _ => {
                let end = token
                    .find(|c: char| c.is_whitespace() || matches!(c, '{' | '}' | '\0'))
                    .unwrap_or(token.len());
                (0, end, end)
            }
        };
        self.0 = &token[advance..];
        Ok(Some(&token[start..end]))
    }
}

fn parse_entities(text: &str) -> Result<Vec<LegacyEntity>, String> {
    let mut tokens = Tokens(text);
    let mut entities = Vec::new();
    while let Some(token) = tokens.next()? {
        if token != "{" {
            return Err("Malformed BSP entity list".into());
        }
        let mut values = HashMap::new();
        loop {
            let key = tokens.next()?.ok_or("Unterminated BSP entity")?;
            if key == "}" {
                break;
            }
            let value = tokens.next()?.ok_or("Missing BSP entity value")?;
            values.insert(key.into(), value.into());
        }
        entities.push(LegacyEntity(values));
    }
    Ok(entities)
}

fn read_optional(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not open {}: {error}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{} exceeds the supported size", path.display()));
    }
    Ok(Some(bytes))
}

fn vector<'a>(mut fields: impl Iterator<Item = &'a str>) -> Option<Vec3> {
    let point = Vec3::new(
        number(fields.next()?)?,
        number(fields.next()?)?,
        number(fields.next()?)?,
    );
    fields.next().is_none().then_some(point)
}

fn number(text: &str) -> Option<f32> {
    text.parse::<f32>().ok().filter(|value| value.is_finite())
}

fn c_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes.split(|&byte| byte == 0).next().unwrap_or_default()).into_owned()
}

fn range<'a, T>(
    data: &'a [T],
    offset: usize,
    count: usize,
    label: &str,
) -> Result<&'a [T], String> {
    data.get(offset..offset.saturating_add(count))
        .ok_or_else(|| format!("Invalid BSP {label} range"))
}

fn records<T>(
    bytes: &[u8],
    width: usize,
    label: &str,
    read: impl FnMut(&[u8]) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    if !bytes.len().is_multiple_of(width) {
        return Err(format!("Invalid BSP {label} record size"));
    }
    bytes.chunks_exact(width).map(read).collect()
}

fn u16_at(bytes: &[u8], at: usize) -> usize {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap()) as usize
}
fn i16_at(bytes: &[u8], at: usize) -> i16 {
    i16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn i32_at(bytes: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn f32_at(bytes: &[u8], at: usize) -> f32 {
    f32::from_bits(u32_at(bytes, at))
}
fn vec_at(bytes: &[u8], at: usize) -> Vec3 {
    Vec3::new(
        f32_at(bytes, at),
        f32_at(bytes, at + 4),
        f32_at(bytes, at + 8),
    )
}
