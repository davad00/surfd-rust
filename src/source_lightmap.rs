use std::collections::HashMap;

use bevy_math::Vec3;

use crate::course::{CourseTexture, Surface, SurfaceLightmap, TextureAlpha};

const PAGE_SIZE: usize = 1024;
const PADDING: usize = 2;
const MAX_PAGES: usize = 32;

static EXPONENT_SCALE: std::sync::LazyLock<[f32; 256]> =
    std::sync::LazyLock::new(|| std::array::from_fn(|index| 2.0_f32.powi(i32::from(index as i8))));

#[derive(Clone, Copy)]
pub(crate) struct FaceLightmap {
    pub offset: i32,
    pub mins: [i32; 2],
    pub extents: [i32; 2],
}

#[derive(Clone, Copy)]
struct Tile {
    page: usize,
    x: usize,
    y: usize,
}

struct Page {
    pixels: Vec<u8>,
    x: usize,
    y: usize,
    row_height: usize,
}

impl Page {
    fn new() -> Self {
        Self {
            pixels: vec![255; PAGE_SIZE * PAGE_SIZE * 4],
            x: 0,
            y: 0,
            row_height: 0,
        }
    }

    fn insert(&mut self, width: usize, height: usize, rgb_exp: &[u8]) -> Option<(usize, usize)> {
        let padded_width = width + PADDING * 2;
        let padded_height = height + PADDING * 2;
        if self.x + padded_width > PAGE_SIZE {
            self.x = 0;
            self.y += self.row_height;
            self.row_height = 0;
        }
        if self.y + padded_height > PAGE_SIZE {
            return None;
        }

        let exponent_scale = &*EXPONENT_SCALE;
        for y in 0..padded_height {
            let sy = y.saturating_sub(PADDING).min(height - 1);
            for x in 0..padded_width {
                let sx = x.saturating_sub(PADDING).min(width - 1);
                let source = 4 * (sy * width + sx);
                let target = 4 * ((self.y + y) * PAGE_SIZE + self.x + x);
                let sample = &rgb_exp[source..source + 4];
                // ColorRGBExp32 encodes a linear channel as byte * 2^exponent / 255.
                let scale = exponent_scale[usize::from(sample[3])];
                for (output, input) in self.pixels[target..target + 3].iter_mut().zip(sample) {
                    *output = (f32::from(*input) * scale).round() as u8;
                }
            }
        }

        let origin = (self.x + PADDING, self.y + PADDING);
        self.x += padded_width;
        self.row_height = self.row_height.max(padded_height);
        Some(origin)
    }
}

pub(crate) struct Lightmaps<'a> {
    data: &'a [u8],
    pages: Vec<Page>,
    tiles: HashMap<(usize, usize, usize), Tile>,
}

impl<'a> Lightmaps<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        // Fullbright maps can still contain a placeholder lighting lump.
        let data = if data.chunks_exact(4).any(|sample| sample[..3] != [0; 3]) {
            data
        } else {
            &[]
        };
        Self {
            data,
            pages: Vec::new(),
            tiles: HashMap::new(),
        }
    }

    pub fn surface(
        &mut self,
        face: FaceLightmap,
        axes: [(Vec3, f32); 2],
        vertices: &[Vec3],
    ) -> Option<SurfaceLightmap> {
        let offset = usize::try_from(face.offset).ok()?;
        let width = usize::try_from(face.extents[0]).ok()?.checked_add(1)?;
        let height = usize::try_from(face.extents[1]).ok()?.checked_add(1)?;
        if width > PAGE_SIZE - PADDING * 2 || height > PAGE_SIZE - PADDING * 2 {
            return None;
        }
        let tile = self.tile(offset, width, height)?;
        let start = [tile.x, tile.y];
        let extents = [width - 1, height - 1];
        let uvs: Vec<_> = vertices
            .iter()
            .map(|&vertex| {
                std::array::from_fn(|axis| {
                    let (direction, shift) = axes[axis];
                    let luxel = vertex.dot(direction) + shift - face.mins[axis] as f32;
                    let pixel = luxel.clamp(0.0, extents[axis] as f32) + start[axis] as f32 + 0.5;
                    pixel / PAGE_SIZE as f32
                })
            })
            .collect();
        uvs.iter()
            .flatten()
            .all(|value| value.is_finite())
            .then_some(SurfaceLightmap {
                index: tile.page,
                uvs,
            })
    }

    fn tile(&mut self, offset: usize, width: usize, height: usize) -> Option<Tile> {
        let key = (offset, width, height);
        if let Some(tile) = self.tiles.get(&key) {
            return Some(*tile);
        }
        let samples = self
            .data
            .get(offset..offset.checked_add(width * height * 4)?)?;
        let last = self
            .pages
            .last_mut()
            .and_then(|page| page.insert(width, height, samples));
        let (x, y) = match last {
            Some(origin) => origin,
            None if self.pages.len() < MAX_PAGES => {
                let mut page = Page::new();
                let origin = page.insert(width, height, samples)?;
                self.pages.push(page);
                origin
            }
            None => return None,
        };
        let tile = Tile {
            page: self.pages.len() - 1,
            x,
            y,
        };
        self.tiles.insert(key, tile);
        Some(tile)
    }

    pub fn append(self, textures: &mut Vec<CourseTexture>, surfaces: &mut [Surface]) {
        let start = textures.len();
        textures.extend(self.pages.into_iter().enumerate().map(|(index, mut page)| {
            let height = (page.y + page.row_height).next_power_of_two();
            page.pixels.truncate(PAGE_SIZE * height * 4);
            CourseTexture {
                bc: None,
                name: format!("source-lightmap-{index}"),
                width: PAGE_SIZE as u32,
                height: height as u32,
                rgba: page.pixels,
                alpha: TextureAlpha::Opaque,
            }
        }));
        for surface in surfaces {
            if let Some(lightmap) = &mut surface.lightmap {
                lightmap.index += start;
                let height = textures[lightmap.index].height;
                if height != PAGE_SIZE as u32 {
                    // Both page heights are powers of two, preserving the sampled texel coordinate.
                    let scale = PAGE_SIZE as f32 / height as f32;
                    for uv in &mut lightmap.uvs {
                        uv[1] *= scale;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entirely_black_lighting_uses_fullbright_materials() {
        let mut maps = Lightmaps::new(&[0; 8]);
        assert!(maps.tile(0, 2, 1).is_none());
        assert!(maps.pages.is_empty());
    }

    #[test]
    fn baked_light_uses_linear_color_and_texel_center_uvs() {
        let mut maps = Lightmaps::new(&[200, 90, 50, 255, 210, 100, 60, 255]);
        let lightmap = maps
            .surface(
                FaceLightmap {
                    offset: 0,
                    mins: [10, 0],
                    extents: [1, 0],
                },
                [(Vec3::X, 0.0), (Vec3::Z, 0.0)],
                &[Vec3::X * 10.0, Vec3::X * 11.0],
            )
            .unwrap();
        let pixel = |x, y| {
            let start = 4 * (y * PAGE_SIZE + x);
            &maps.pages[0].pixels[start..start + 4]
        };
        assert_eq!(lightmap.uvs[0], [2.5 / 1024.0; 2]);
        assert_eq!(lightmap.uvs[1], [3.5 / 1024.0, 2.5 / 1024.0]);
        assert_eq!(pixel(0, 0), [100, 45, 25, 255]);
        assert_eq!(pixel(3, 2), [105, 50, 30, 255]);
    }

    #[test]
    fn trimmed_lightmap_retains_padding_and_samples_the_same_texels() {
        let mut maps = Lightmaps::new(&[200, 90, 50, 255, 210, 100, 60, 255]);
        let lightmap = maps
            .surface(
                FaceLightmap {
                    offset: 0,
                    mins: [0; 2],
                    extents: [0, 1],
                },
                [(Vec3::X, 0.0), (Vec3::Z, 0.0)],
                &[Vec3::ZERO, Vec3::Z * 0.375, Vec3::Z],
            )
            .unwrap();
        let old_uvs = lightmap.uvs.clone();
        let mut surfaces = [Surface {
            vertices: Vec::new(),
            color: [1.0; 3],
            texture: None,
            lightmap: Some(lightmap),
        }];
        let old_pixels = maps.pages[0].pixels[..PAGE_SIZE * 8 * 4].to_vec();
        let old_allocation = maps.pages[0].pixels.as_ptr();
        let mut textures = Vec::new();
        maps.append(&mut textures, &mut surfaces);

        let texture = &textures[0];
        assert_eq!((texture.width, texture.height), (PAGE_SIZE as u32, 8));
        assert_eq!(texture.rgba, old_pixels);
        assert_eq!(texture.rgba.as_ptr(), old_allocation);
        let lightmap = surfaces[0].lightmap.as_ref().unwrap();
        for (old, new) in old_uvs.iter().zip(&lightmap.uvs) {
            assert_eq!(old[0], new[0]);
            assert_eq!(old[1] * PAGE_SIZE as f32, new[1] * texture.height as f32);
        }
    }
}
