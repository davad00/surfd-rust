//! Original replacement art for imported maps with unavailable material assets.

use bevy_math::Vec3;

use crate::course::{Course, CourseTexture, Surface, SurfaceTexture, TextureAlpha};

/// The renderer uses these names to apply the BSP tint and directional shading.
pub const REPLACEMENT_SURFACES: [&str; 2] = ["surfd-fallback/surface", "surfd-fallback/glass"];

pub(crate) fn fill_missing_surfaces(course: &mut Course) {
    let mut replacements = [None; 2];
    for surface in &mut course.surfaces {
        fill_surface(surface, &mut course.textures, &mut replacements, false);
    }
}

pub(crate) fn fill_surface(
    surface: &mut Surface,
    textures: &mut Vec<CourseTexture>,
    replacements: &mut [Option<usize>; 2],
    translucent: bool,
) {
    if surface.texture.is_some() {
        return;
    }
    let Some(uvs) = projected_uvs(surface) else {
        return;
    };
    let index = *replacements[usize::from(translucent)].get_or_insert_with(|| {
        let index = textures.len();
        textures.push(surface_texture(translucent));
        index
    });
    surface.texture = Some(SurfaceTexture { index, uvs });
    // Missing materials can have almost black baked light. Shade this replacement
    // by face orientation so players can still see the course's collision geometry.
    surface.lightmap = None;
}

fn projected_uvs(surface: &Surface) -> Option<Vec<[f32; 2]>> {
    const TILE_UNITS: f32 = 128.0;
    let (&origin, rest) = surface.vertices.split_first()?;
    let normal = rest
        .windows(2)
        .map(|pair| (pair[0] - origin).cross(pair[1] - origin))
        .sum::<Vec3>()
        .try_normalize()?;
    let reference = if normal.y.abs() < 0.9 {
        Vec3::Y
    } else {
        Vec3::Z
    };
    let across = normal.cross(reference).normalize();
    let along = across.cross(normal);
    let uvs: Vec<_> = surface
        .vertices
        .iter()
        .map(|point| {
            [
                point.dot(across) / TILE_UNITS,
                point.dot(along) / TILE_UNITS,
            ]
        })
        .collect();
    uvs.iter()
        .flatten()
        .all(|value| value.is_finite())
        .then_some(uvs)
}

fn surface_texture(translucent: bool) -> CourseTexture {
    const SIZE: u32 = 64;
    let image = image::RgbaImage::from_fn(SIZE, SIZE, |x, y| {
        let grain = (((x * 149) ^ (y * 223) ^ (x * y * 17)) % 13) as u8;
        let edge = x.min(y);
        let value = match edge {
            0 => 126,
            1 => 153,
            _ => 215 + grain,
        };
        let alpha = match (translucent, edge) {
            (false, _) => 255,
            (true, 0..=1) => 124,
            (true, _) => 66,
        };
        image::Rgba([value, value, value, alpha])
    });
    let alpha = if translucent {
        TextureAlpha::Blend
    } else {
        TextureAlpha::Opaque
    };
    texture(
        REPLACEMENT_SURFACES[usize::from(translucent)].into(),
        image,
        alpha,
    )
}

pub(crate) fn append_sky(textures: &mut Vec<CourseTexture>) -> [usize; 6] {
    const SIZE: u32 = 64;
    let start = textures.len();
    for face in 0..6 {
        let image = image::RgbaImage::from_fn(SIZE, SIZE, |x, y| {
            let u = 2.0 * (x as f32 + 0.5) / SIZE as f32 - 1.0;
            let v = 2.0 * (y as f32 + 0.5) / SIZE as f32 - 1.0;
            let height = match face {
                2 => 1.0,
                3 => -1.0,
                _ => -v,
            } / (1.0 + u * u + v * v).sqrt();
            let horizon = [152.0_f32, 173.0, 191.0];
            let pole = if height >= 0.0 {
                [39.0, 81.0, 122.0]
            } else {
                [65.0, 77.0, 93.0]
            };
            let blend = height.abs().sqrt();
            let [r, g, b] = std::array::from_fn(|i| {
                (horizon[i] + (pole[i] - horizon[i]) * blend).round() as u8
            });
            image::Rgba([r, g, b, 255])
        });
        textures.push(texture(
            format!("surfd-fallback/sky/{face}"),
            image,
            TextureAlpha::Opaque,
        ));
    }
    std::array::from_fn(|face| start + face)
}

fn texture(name: String, image: image::RgbaImage, alpha: TextureAlpha) -> CourseTexture {
    CourseTexture {
        name,
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
        bc: None,
        alpha,
    }
}
