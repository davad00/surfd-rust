//! GoldSrc BSP30 assets. Geometry and collision share the Quake BSP reader.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::course::{Course, CourseTexture, TextureAlpha};
use crate::quake_bsp::LegacyBsp;
use crate::source_bsp::{map_name, read_bsp_bytes};

const MAX_TEXTURES: usize = 8192;
const MAX_WAD_ENTRIES: usize = 65536;
const MAX_DIMENSION: usize = 2048;
const MAX_RGBA_BYTES: usize = 256 * 1024 * 1024;

pub fn load(path: &Path) -> Result<Course, String> {
    let result = (|| {
        let bytes = read_bsp_bytes(path)?;
        let bsp = LegacyBsp::parse(&bytes, 30)?;
        let name = map_name(path).ok_or("Expected a GoldSrc BSP filename")?;
        let wad_list = bsp
            .entities()
            .first()
            .and_then(|entity| entity.get("wad"))
            .unwrap_or_default();
        let mut wads = open_wads(path, wad_list);
        bsp.build(name, textures(bsp.texture_lump(), &mut wads)?)
    })();
    result.map_err(|error: String| format!("{}: {error}", path.display()))
}

fn textures(bytes: &[u8], wads: &mut [Wad]) -> Result<Vec<Option<CourseTexture>>, String> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let count = u32_at(bytes, 0).ok_or("GoldSrc miptex directory is truncated")? as usize;
    if count > MAX_TEXTURES || bytes.len() < 4 + count * 4 {
        return Err("GoldSrc miptex directory has an invalid size".into());
    }
    let mut remaining = MAX_RGBA_BYTES;
    Ok((0..count)
        .map(|index| {
            let offset = u32_at(bytes, 4 + index * 4)? as usize;
            let mip = bytes.get(offset..)?;
            let name = fixed_name(mip.get(..16)?)?;
            let texture = decode_miptex(mip, &name, remaining).or_else(|| {
                wads.iter_mut().find_map(|wad| {
                    let data = wad.read(&name)?;
                    decode_miptex(&data, &name, remaining)
                })
            })?;
            remaining -= texture.rgba.len();
            Some(texture)
        })
        .collect())
}

fn decode_miptex(bytes: &[u8], name: &str, remaining: usize) -> Option<CourseTexture> {
    let width = u32_at(bytes, 16)? as usize;
    let height = u32_at(bytes, 20)? as usize;
    if !(16..=MAX_DIMENSION).contains(&width) || !(16..=MAX_DIMENSION).contains(&height) {
        return None;
    }
    let area = width * height;
    if area * 4 > remaining {
        return None;
    }
    let first = u32_at(bytes, 24)? as usize;
    let last = u32_at(bytes, 36)? as usize;
    if first < 40 || last < first + area {
        return None;
    }
    let indices = bytes.get(first..first.checked_add(area)?)?;
    let palette_offset = last.checked_add(area / 64)?;
    let count = u16::from_le_bytes(*bytes.get(palette_offset..)?.first_chunk()?) as usize;
    if count != 256 {
        return None;
    }
    let palette = bytes.get(palette_offset + 2..palette_offset + 2 + 3 * count)?;
    let masked = name.starts_with('{');
    let mut rgba = Vec::with_capacity(area * 4);
    for &index in indices {
        let color = usize::from(index) * 3;
        rgba.extend_from_slice(&palette[color..color + 3]);
        rgba.push(if masked && index == 255 { 0 } else { 255 });
    }
    Some(CourseTexture {
        name: name.to_owned(),
        width: width as u32,
        height: height as u32,
        rgba,
        bc: None,
        alpha: if masked {
            TextureAlpha::Cutout
        } else {
            TextureAlpha::Opaque
        },
    })
}

fn open_wads(map: &Path, listed: &str) -> Vec<Wad> {
    let mut roots = Vec::new();
    if let Some(parent) = map.parent() {
        roots.push(parent.to_path_buf());
        roots.extend(parent.parent().map(Path::to_path_buf));
    }
    if let Some(configured) = std::env::var_os("SURF_GOLDSRC_ASSETS") {
        roots.extend(std::env::split_paths(&configured));
    }
    let names = listed.split(';').filter_map(|path| {
        let name = path.rsplit(['/', '\\']).next()?.trim();
        (name.len() < 96
            && name.to_ascii_lowercase().ends_with(".wad")
            && name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b' ')
            }))
        .then_some(name)
    });
    let mut found = std::collections::HashSet::<PathBuf>::new();
    for root in &roots {
        if root
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("wad"))
            && root.is_file()
        {
            found.insert(root.clone());
        }
    }
    for name in names.take(64) {
        for root in &roots {
            let candidates = [
                root.join(name),
                root.join("cstrike").join(name),
                root.join("valve").join(name),
            ];
            if let Some(path) = candidates.into_iter().find(|path| path.is_file()) {
                found.insert(path);
                break;
            }
        }
    }
    let mut found: Vec<_> = found.into_iter().collect();
    found.sort_unstable();
    found
        .into_iter()
        .filter_map(|path| Wad::open(&path))
        .collect()
}

struct Wad {
    file: File,
    entries: HashMap<String, (u64, usize)>,
}

impl Wad {
    fn open(path: &Path) -> Option<Self> {
        let mut file = File::open(path).ok()?;
        let length = file.metadata().ok()?.len();
        let mut header = [0; 12];
        file.read_exact(&mut header).ok()?;
        if &header[..4] != b"WAD3" {
            return None;
        }
        let count = u32_at(&header, 4)? as usize;
        let start = u64::from(u32_at(&header, 8)?);
        if count > MAX_WAD_ENTRIES || start < 12 || start + (count * 32) as u64 > length {
            return None;
        }
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut entries = HashMap::new();
        let mut row = [0; 32];
        for _ in 0..count {
            file.read_exact(&mut row).ok()?;
            let offset = u64::from(u32_at(&row, 0)?);
            let size = u32_at(&row, 4)? as usize;
            if row[12] != 0x43
                || row[13] != 0
                || size > 16 * 1024 * 1024
                || offset < 12
                || offset + size as u64 > length
            {
                continue;
            }
            if let Some(name) = fixed_name(&row[16..]) {
                entries.entry(name).or_insert((offset, size));
            }
        }
        Some(Self { file, entries })
    }

    fn read(&mut self, name: &str) -> Option<Vec<u8>> {
        let &(offset, size) = self.entries.get(name)?;
        self.file.seek(SeekFrom::Start(offset)).ok()?;
        let mut bytes = vec![0; size];
        self.file.read_exact(&mut bytes).ok()?;
        Some(bytes)
    }
}

fn u32_at(bytes: &[u8], index: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*bytes.get(index..)?.first_chunk()?))
}

fn fixed_name(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    (end > 0).then(|| String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_wad3_palette_and_masked_transparency() {
        let mut bytes = vec![0; 40 + 256 + 64 + 16 + 4 + 2 + 768];
        for (at, value) in [(16, 16_u32), (20, 16), (24, 40), (36, 376)] {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[40] = 7;
        bytes[41] = 255;
        bytes[380..382].copy_from_slice(&256_u16.to_le_bytes());
        bytes[403..406].copy_from_slice(&[10, 20, 30]);
        let texture = decode_miptex(&bytes, "{fence", 4096).unwrap();
        assert_eq!(&texture.rgba[..8], &[10, 20, 30, 255, 0, 0, 0, 0]);
        assert!(matches!(texture.alpha, TextureAlpha::Cutout));
        assert!(decode_miptex(&bytes, "{fence", 1000).is_none());
    }
}
