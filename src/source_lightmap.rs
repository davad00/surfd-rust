use std::collections::{BTreeMap, BTreeSet, HashMap};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tile {
    page: usize,
    x: usize,
    y: usize,
}

#[derive(Default)]
struct ShelfPage {
    x: usize,
    y: usize,
    row_height: usize,
}

impl ShelfPage {
    fn insert(&mut self, width: usize, height: usize) -> Option<(usize, usize)> {
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

        let origin = (self.x + PADDING, self.y + PADDING);
        self.x += padded_width;
        self.row_height = self.row_height.max(padded_height);
        Some(origin)
    }

    fn height(&self) -> usize {
        (self.y + self.row_height).next_power_of_two()
    }
}

struct TileSource {
    offset: usize,
    width: usize,
    height: usize,
    position: Tile,
}

fn write_tile(pixels: &mut [u8], source: &TileSource, tile: Tile, data: &[u8]) {
    let TileSource {
        offset,
        width,
        height,
        ..
    } = *source;
    let padded_width = width + PADDING * 2;
    let padded_height = height + PADDING * 2;
    let exponent_scale = &*EXPONENT_SCALE;
    for y in 0..padded_height {
        let sy = y.saturating_sub(PADDING).min(height - 1);
        for x in 0..padded_width {
            let sx = x.saturating_sub(PADDING).min(width - 1);
            let start = offset + 4 * (sy * width + sx);
            let target = 4 * ((tile.y - PADDING + y) * PAGE_SIZE + tile.x - PADDING + x);
            let sample = &data[start..start + 4];
            // ColorRGBExp32 encodes a linear channel as byte * 2^exponent / 255.
            let scale = exponent_scale[usize::from(sample[3])];
            for (output, input) in pixels[target..target + 3].iter_mut().zip(sample) {
                *output = (f32::from(*input) * scale).round() as u8;
            }
        }
    }
}

fn pack_tiles(sources: &[TileSource]) -> Option<(Vec<usize>, Vec<Tile>)> {
    let mut order: Vec<_> = (0..sources.len()).collect();
    order.sort_unstable_by_key(|&i| {
        (
            std::cmp::Reverse(sources[i].height),
            std::cmp::Reverse(sources[i].width),
            i,
        )
    });
    let mut page_heights: Vec<usize> = Vec::new();
    let mut shelves = BTreeMap::<usize, BTreeSet<(usize, usize, usize)>>::new();
    let mut positions: Vec<_> = sources.iter().map(|source| source.position).collect();
    for index in order {
        let source = &sources[index];
        let width = source.width + PADDING * 2;
        let height = source.height + PADDING * 2;
        let shelf = shelves.range_mut(height..).find_map(|(_, shelves)| {
            let &(available, page, y) = shelves.range((width, 0, 0)..).next()?;
            shelves.remove(&(available, page, y));
            shelves.insert((available - width, page, y));
            Some((page, PAGE_SIZE - available, y))
        });
        let (page, x, y) = if let Some(shelf) = shelf {
            shelf
        } else {
            let page = page_heights
                .iter()
                .position(|used| used + height <= PAGE_SIZE)
                .unwrap_or(page_heights.len());
            if page == MAX_PAGES {
                return None;
            }
            if page == page_heights.len() {
                page_heights.push(0);
            }
            let y = page_heights[page];
            page_heights[page] += height;
            shelves
                .entry(height)
                .or_default()
                .insert((PAGE_SIZE - width, page, y));
            (page, 0, y)
        };
        positions[index] = Tile {
            page,
            x: x + PADDING,
            y: y + PADDING,
        };
    }
    for height in &mut page_heights {
        *height = height.next_power_of_two();
    }
    Some((page_heights, positions))
}

pub(crate) struct Lightmaps<'a> {
    data: &'a [u8],
    pages: Vec<ShelfPage>,
    tiles: HashMap<(usize, usize, usize), usize>,
    sources: Vec<TileSource>,
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
            sources: Vec::new(),
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
        let extents = [width - 1, height - 1];
        let uvs: Vec<_> = vertices
            .iter()
            .map(|&vertex| {
                std::array::from_fn(|axis| {
                    let (direction, shift) = axes[axis];
                    let luxel = vertex.dot(direction) + shift - face.mins[axis] as f32;
                    luxel.clamp(0.0, extents[axis] as f32)
                })
            })
            .collect();
        uvs.iter()
            .flatten()
            .all(|value| value.is_finite())
            .then_some(SurfaceLightmap { index: tile, uvs })
    }

    fn tile(&mut self, offset: usize, width: usize, height: usize) -> Option<usize> {
        let key = (offset, width, height);
        if let Some(tile) = self.tiles.get(&key) {
            return Some(*tile);
        }
        self.data
            .get(offset..offset.checked_add(width * height * 4)?)?;
        let last = self
            .pages
            .last_mut()
            .and_then(|page| page.insert(width, height));
        let (x, y) = match last {
            Some(origin) => origin,
            None if self.pages.len() < MAX_PAGES => {
                let mut page = ShelfPage::default();
                let origin = page.insert(width, height)?;
                self.pages.push(page);
                origin
            }
            None => return None,
        };
        let index = self.sources.len();
        self.sources.push(TileSource {
            offset,
            width,
            height,
            position: Tile {
                page: self.pages.len() - 1,
                x,
                y,
            },
        });
        self.tiles.insert(key, index);
        Some(index)
    }

    pub fn append(self, textures: &mut Vec<CourseTexture>, surfaces: &mut [Surface]) {
        let start = textures.len();
        let legacy: Vec<_> = self.pages.iter().map(ShelfPage::height).collect();
        let (heights, positions) = pack_tiles(&self.sources)
            .filter(|(heights, _)| heights.iter().sum::<usize>() < legacy.iter().sum::<usize>())
            .unwrap_or_else(|| {
                (
                    legacy,
                    self.sources.iter().map(|source| source.position).collect(),
                )
            });
        textures.extend(
            heights
                .iter()
                .enumerate()
                .map(|(index, &height)| CourseTexture {
                    bc: None,
                    name: format!("source-lightmap-{index}"),
                    width: PAGE_SIZE as u32,
                    height: height as u32,
                    rgba: vec![255; PAGE_SIZE * height * 4],
                    alpha: TextureAlpha::Opaque,
                }),
        );
        for (source, &tile) in self.sources.iter().zip(&positions) {
            write_tile(
                &mut textures[start + tile.page].rgba,
                source,
                tile,
                self.data,
            );
        }
        for surface in surfaces {
            if let Some(lightmap) = &mut surface.lightmap {
                let tile = positions[lightmap.index];
                lightmap.index = start + tile.page;
                for uv in &mut lightmap.uvs {
                    uv[0] = (uv[0] + tile.x as f32 + 0.5) / PAGE_SIZE as f32;
                    uv[1] = (uv[1] + tile.y as f32 + 0.5) / heights[tile.page] as f32;
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
        let mut surfaces = [Surface {
            vertices: Vec::new(),
            color: [1.0; 3],
            texture: None,
            lightmap: Some(lightmap),
        }];
        let mut textures = Vec::new();
        maps.append(&mut textures, &mut surfaces);
        let lightmap = surfaces[0].lightmap.as_ref().unwrap();
        let pixel = |x, y| {
            let start = 4 * (y * PAGE_SIZE + x);
            &textures[0].rgba[start..start + 4]
        };
        assert_eq!(lightmap.uvs[0], [2.5 / 1024.0, 2.5 / 8.0]);
        assert_eq!(lightmap.uvs[1], [3.5 / 1024.0, 2.5 / 8.0]);
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
        let mut surfaces = [Surface {
            vertices: Vec::new(),
            color: [1.0; 3],
            texture: None,
            lightmap: Some(lightmap),
        }];
        let mut textures = Vec::new();
        maps.append(&mut textures, &mut surfaces);

        let texture = &textures[0];
        assert_eq!((texture.width, texture.height), (PAGE_SIZE as u32, 8));
        assert_eq!(texture.rgba.len(), PAGE_SIZE * 8 * 4);
        let bottom_pad = 4 * (PAGE_SIZE * 5 + 2);
        assert_eq!(
            &texture.rgba[bottom_pad..bottom_pad + 4],
            [105, 50, 30, 255]
        );
        let lightmap = surfaces[0].lightmap.as_ref().unwrap();
        for (luxel, uv) in [0.0, 0.375, 1.0].into_iter().zip(&lightmap.uvs) {
            assert_eq!(uv[0], 2.5 / PAGE_SIZE as f32);
            assert_eq!(uv[1] * texture.height as f32, luxel + 2.5);
        }
    }

    #[test]
    fn sorted_shelves_save_a_page_and_preserve_each_tiles_samples() {
        let mut data = Vec::new();
        let mut tiles = Vec::new();
        for (width, height, color) in [(396, 596, 32), (596, 276, 64), (596, 596, 96)] {
            let offset = data.len();
            for _ in 0..width * height {
                data.extend([color, 1, 2, 0]);
            }
            tiles.push((offset, width, height, color));
        }
        let mut maps = Lightmaps::new(&data);
        let mut surfaces = Vec::new();
        for (offset, width, height, _) in &tiles {
            let index = maps.tile(*offset, *width, *height).unwrap();
            surfaces.push(Surface {
                vertices: Vec::new(),
                color: [1.0; 3],
                texture: None,
                lightmap: Some(SurfaceLightmap {
                    index,
                    uvs: vec![[0.0, 0.0]],
                }),
            });
        }
        assert_eq!(maps.pages.len(), 2);
        let mut textures = Vec::new();
        maps.append(&mut textures, &mut surfaces);
        assert_eq!(textures.len(), 1);
        for (surface, (_, _, _, color)) in surfaces.iter().zip(tiles) {
            let lightmap = surface.lightmap.as_ref().unwrap();
            let texture = &textures[lightmap.index];
            let [u, v] = lightmap.uvs[0];
            let x = (u * texture.width as f32) as usize;
            let y = (v * texture.height as f32) as usize;
            let at = 4 * (y * texture.width as usize + x);
            assert_eq!(&texture.rgba[at..at + 4], [color, 1, 2, 255]);
        }
    }
}
