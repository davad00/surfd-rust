//! Resolves Source materials from the map pakfile, loose files and game VPKs.

use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use lzma_rs::decompress::{Options, UnpackedSize};
use vformats::bsp::{ZipEntry, ZipReader};
use vformats::vpk::{self, VpkLocation};
use vformats::{Limits, vmt, vtf};

use crate::bc_texture::BcTexture;
use crate::course::{CourseTexture, Surface, TextureAlpha};
use crate::map_materials;

const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DECODED_BYTES: usize = 256 * 1024 * 1024;
const MAX_PATCH_DEPTH: u8 = 8;
const MAX_SKY_FACE: u32 = 1024;

pub(crate) struct Materials<'a> {
    pak: Option<ZipReader<'a>>,
    pak_index: HashMap<String, usize>,
    pak_lzma: Vec<Option<&'a [u8]>>,
    roots: Vec<PathBuf>,
    archives: Option<Vec<Archive>>,
    limits: Limits,
    resolved: HashMap<String, Option<Resolved>>,
    by_image: HashMap<(String, Alpha), usize>,
    textures: Vec<CourseTexture>,
    decoded_bytes: usize,
    use_bc: bool,
    replacements: [Option<usize>; 2],
}

#[derive(Clone, Copy)]
pub(crate) struct Resolved {
    pub index: usize,
    pub lightmapped: bool,
    transform: UvTransform,
}

impl Resolved {
    pub fn uv(self, point: [f32; 2]) -> [f32; 2] {
        self.transform.apply(point)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum Alpha {
    #[default]
    Opaque,
    Cutout,
    Blend,
}

impl From<Alpha> for TextureAlpha {
    fn from(alpha: Alpha) -> Self {
        match alpha {
            Alpha::Opaque => Self::Opaque,
            Alpha::Cutout => Self::Cutout,
            Alpha::Blend => Self::Blend,
        }
    }
}

#[derive(Clone, Copy)]
struct UvTransform {
    center: [f32; 2],
    scale: [f32; 2],
    rotation: [f32; 2],
    translation: [f32; 2],
}

impl Default for UvTransform {
    fn default() -> Self {
        Self {
            center: [0.5; 2],
            scale: [1.0; 2],
            rotation: [0.0, 1.0],
            translation: [0.0; 2],
        }
    }
}

impl UvTransform {
    fn parse(value: &str) -> Option<Self> {
        let mut fields = value.split_ascii_whitespace();
        let mut transform = Self::default();
        while let Some(key) = fields.next() {
            let mut number = || {
                let value = fields.next()?.parse::<f32>().ok()?;
                value.is_finite().then_some(value)
            };
            match key.to_ascii_lowercase().as_str() {
                "center" => transform.center = [number()?, number()?],
                "scale" => transform.scale = [number()?, number()?],
                "rotate" => {
                    let (sin, cos) = number()?.to_radians().sin_cos();
                    transform.rotation = [sin, cos];
                }
                "translate" => transform.translation = [number()?, number()?],
                _ => return None,
            }
        }
        Some(transform)
    }

    fn apply(self, point: [f32; 2]) -> [f32; 2] {
        let x = (point[0] - self.center[0]) * self.scale[0];
        let y = (point[1] - self.center[1]) * self.scale[1];
        let [sin, cos] = self.rotation;
        [
            x * cos - y * sin + self.center[0] + self.translation[0],
            x * sin + y * cos + self.center[1] + self.translation[1],
        ]
    }
}

#[derive(Default)]
struct Definition {
    base: Option<String>,
    translucent: bool,
    additive: bool,
    cutout: bool,
    unlit: bool,
    transform: UvTransform,
}

impl Definition {
    fn apply<'a>(&mut self, get: impl Fn(&str) -> Option<&'a str>) {
        if let Some(base) = get("$basetexture") {
            self.base = material_path(base, ".vtf");
        }
        if let Some(value) = get("$translucent") {
            self.translucent = enabled(value);
        }
        if let Some(value) = get("$additive") {
            self.additive = enabled(value);
        }
        if let Some(value) = get("$alphatest") {
            self.cutout = enabled(value);
        }
        if let Some(transform) = get("$basetexturetransform").and_then(UvTransform::parse) {
            self.transform = transform;
        }
    }

    fn alpha(&self) -> Alpha {
        if self.cutout {
            Alpha::Cutout
        } else if self.translucent || self.additive {
            Alpha::Blend
        } else {
            Alpha::Opaque
        }
    }
}

fn enabled(value: &str) -> bool {
    value.trim().parse::<f32>().is_ok_and(|number| number > 0.0)
}

impl<'a> Materials<'a> {
    pub fn new(pak_bytes: &'a [u8], map_path: Option<&Path>, use_bc: bool) -> Self {
        let pak = ZipReader::parse(pak_bytes).ok();
        let pak_index = pak
            .as_ref()
            .into_iter()
            .flat_map(ZipReader::entries)
            .enumerate()
            .filter_map(|(index, entry)| pak_entry_path(entry).map(|path| (path, index)))
            .collect();
        let pak_lzma = pak
            .as_ref()
            .and_then(|pak| zip_lzma_payloads(pak_bytes, pak.entries().len()))
            .unwrap_or_default();
        Self {
            pak,
            pak_index,
            pak_lzma,
            roots: asset_roots(map_path),
            archives: None,
            limits: Limits {
                max_entry_bytes: MAX_ENTRY_BYTES,
                ..Limits::default()
            },
            resolved: HashMap::new(),
            by_image: HashMap::new(),
            textures: Vec::new(),
            decoded_bytes: 0,
            use_bc,
            replacements: [None; 2],
        }
    }

    pub fn resolve(&mut self, name: &str) -> Option<Resolved> {
        if let Some(texture) = self.resolved.get(name) {
            return *texture;
        }
        let texture = self.resolve_material(name);
        self.resolved.insert(name.to_owned(), texture);
        texture
    }

    pub fn into_textures(self) -> Vec<CourseTexture> {
        self.textures
    }

    pub fn texture_dimensions(&self, resolved: Resolved) -> [f32; 2] {
        let texture = &self.textures[resolved.index];
        [texture.width as f32, texture.height as f32]
    }

    pub fn fill_surface(&mut self, surface: &mut Surface, translucent: bool) {
        map_materials::fill_surface(
            surface,
            &mut self.textures,
            &mut self.replacements,
            translucent,
        );
    }

    pub fn resolve_sky(&mut self, name: &str) -> Option<[usize; 6]> {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return None;
        }
        // World rays are converted from (x, z, -y) and Bevy flips Z for cubemap sampling.
        // The top and bottom Source textures use rotated UV axes.
        let mut inputs = Vec::new();
        for suffix in ["rt", "lf", "up", "dn", "bk", "ft"] {
            let stem = format!("skybox/{name}{suffix}");
            let vmt = material_path(&stem, ".vmt")?;
            let definition = self.definition(&vmt, MAX_PATCH_DEPTH).unwrap_or_default();
            let path = definition.base.or_else(|| material_path(&stem, ".vtf"))?;
            let bytes = self.read(&path)?;
            let texture = vtf::parse(&bytes, &self.limits).ok()?;
            let largest = texture.width().max(texture.height());
            let mip = largest.div_ceil(MAX_SKY_FACE).next_power_of_two().ilog2() as u8;
            let decoded = texture
                .decode_rgba(0, 0, mip, &self.limits)
                .or_else(|_| texture.decode_rgba(0, 0, 0, &self.limits))
                .ok()?;
            let mut image =
                image::RgbaImage::from_raw(decoded.width, decoded.height, decoded.rgba)?;
            if image.width().max(image.height()) > MAX_SKY_FACE {
                let scale = MAX_SKY_FACE as f32 / image.width().max(image.height()) as f32;
                image = image::imageops::resize(
                    &image,
                    (image.width() as f32 * scale).round().max(1.0) as u32,
                    (image.height() as f32 * scale).round().max(1.0) as u32,
                    image::imageops::FilterType::Triangle,
                );
            }
            inputs.push((path, image, definition.transform));
        }
        let size = inputs
            .iter()
            .map(|(_, image, _)| image.width().max(image.height()))
            .max()?;
        let start = self.textures.len();
        for (face, (path, image, transform)) in inputs.into_iter().enumerate() {
            let face = image::RgbaImage::from_fn(size, size, |x, y| {
                let uv = sky_face_uv(
                    face,
                    [
                        (x as f32 + 0.5) / size as f32,
                        (y as f32 + 0.5) / size as f32,
                    ],
                );
                let [u, v] = transform.apply(uv);
                let x = ((u * image.width() as f32) as u32).min(image.width() - 1);
                let y = ((v * image.height() as f32) as u32).min(image.height() - 1);
                let [r, g, b, _] = image.get_pixel(x, y).0;
                image::Rgba([r, g, b, 255])
            });
            self.textures.push(CourseTexture {
                name: path,
                width: size,
                height: size,
                rgba: face.into_raw(),
                bc: None,
                alpha: TextureAlpha::Opaque,
            });
        }
        Some(std::array::from_fn(|face| start + face))
    }

    fn resolve_material(&mut self, name: &str) -> Option<Resolved> {
        let path = material_path(name, ".vmt")?;
        let definition = self.definition(&path, MAX_PATCH_DEPTH).unwrap_or_default();
        let alpha = definition.alpha();
        let path = definition.base.or_else(|| material_path(name, ".vtf"))?;
        let key = (path, alpha);
        let index = if let Some(index) = self.by_image.get(&key) {
            *index
        } else {
            let bytes = self.read(&key.0)?;
            let texture = vtf::parse(&bytes, &self.limits).ok()?;
            let output_bytes = texture.width() as usize * texture.height() as usize * 4;
            // Keep map material admission identical on compressed and decoded adapters.
            if output_bytes as u64 > self.limits.max_entry_bytes
                || self.decoded_bytes.saturating_add(output_bytes) > MAX_DECODED_BYTES
            {
                return None;
            }
            let bc = self.use_bc.then(|| BcTexture::from_vtf(&texture)).flatten();
            let rgba = if bc.is_none() {
                texture.decode_rgba(0, 0, 0, &self.limits).ok()?.rgba
            } else {
                Vec::new()
            };
            self.decoded_bytes += output_bytes;
            let index = self.textures.len();
            self.textures.push(CourseTexture {
                bc,
                name: key.0.clone(),
                width: texture.width(),
                height: texture.height(),
                rgba,
                alpha: alpha.into(),
            });
            self.by_image.insert(key, index);
            index
        };
        Some(Resolved {
            index,
            lightmapped: !definition.unlit,
            transform: definition.transform,
        })
    }

    fn definition(&mut self, path: &str, depth: u8) -> Option<Definition> {
        if depth == 0 {
            return None;
        }
        let bytes = self.read(path)?;
        let text = String::from_utf8_lossy(&bytes);
        let doc = vmt::parse(
            text.trim_matches('\u{feff}').trim_end_matches('\0'),
            &self.limits,
        )
        .ok()?;
        if let Some(patch) = doc.patch() {
            let mut definition = material_path(&patch.include, ".vmt")
                .and_then(|include| self.definition(&include, depth - 1))
                .unwrap_or_default();
            definition.apply(|key| patch.value(key));
            Some(definition)
        } else {
            let mut definition = Definition {
                unlit: doc.shader.to_ascii_lowercase().starts_with("unlit"),
                ..Default::default()
            };
            definition.apply(|key| doc.value(key));
            Some(definition)
        }
    }

    fn read(&mut self, path: &str) -> Option<Cow<'a, [u8]>> {
        if let Some(bytes) = self.pak.as_ref().and_then(|pak| {
            let index = *self.pak_index.get(path)?;
            let entry = pak.entries().get(index)?;
            if let Some(payload) = self.pak_lzma.get(index).copied().flatten() {
                unpack_zip_lzma(payload, entry.uncompressed_size, entry.crc32).map(Cow::Owned)
            } else {
                pak.entry_bytes(entry, &self.limits).ok()
            }
        }) {
            return Some(bytes);
        }
        if let Some(bytes) = self
            .roots
            .iter()
            .find_map(|root| read_bounded(&root.join(path), MAX_ENTRY_BYTES))
        {
            return Some(Cow::Owned(bytes));
        }
        self.archives
            .get_or_insert_with(|| archives(&self.roots, &self.limits))
            .iter()
            .find_map(|archive| archive.read(path))
            .map(Cow::Owned)
    }
}

fn sky_face_uv(face: usize, [u, v]: [f32; 2]) -> [f32; 2] {
    match face {
        2 => [1.0 - v, u],
        3 => [v, 1.0 - u],
        _ => [u, v],
    }
}

// Keep ZipReader's paths and directory order. Look up raw payloads for method 14,
// whose compressed offsets are private to that reader.
fn zip_lzma_payloads(bytes: &[u8], count: usize) -> Option<Vec<Option<&[u8]>>> {
    if count == 0 {
        return Some(Vec::new());
    }
    let last_eocd = bytes.len().checked_sub(22)?;
    let eocd = (last_eocd.saturating_sub(usize::from(u16::MAX))..=last_eocd)
        .rev()
        .find(|&offset| bytes[offset..].starts_with(b"PK\x05\x06"))?;
    let directory_offset = zip_u32(&bytes[eocd..], 16)? as usize;
    let local_data = bytes.get(..directory_offset)?;
    let mut directory = bytes.get(directory_offset..eocd)?;
    let mut payloads = Vec::with_capacity(count);
    for _ in 0..count {
        if !directory.starts_with(b"PK\x01\x02") {
            return None;
        }
        let name_len = usize::from(zip_u16(directory, 28)?);
        let extra_len = usize::from(zip_u16(directory, 30)?);
        let comment_len = usize::from(zip_u16(directory, 32)?);
        let payload = if zip_u16(directory, 10)? == 14 && zip_u16(directory, 8)? & 1 == 0 {
            let local = zip_u32(directory, 42)? as usize;
            let size = zip_u32(directory, 20)? as usize;
            zip_lzma_payload(local_data, local, size)
        } else {
            None
        };
        payloads.push(payload);
        directory = directory.get(46 + name_len + extra_len + comment_len..)?;
    }
    Some(payloads)
}

fn zip_lzma_payload(bytes: &[u8], local: usize, size: usize) -> Option<&[u8]> {
    let header = bytes.get(local..)?;
    if !header.starts_with(b"PK\x03\x04")
        || zip_u16(header, 6)? & 1 != 0
        || zip_u16(header, 8)? != 14
        || size as u64 > MAX_ENTRY_BYTES
    {
        return None;
    }
    let start = 30 + usize::from(zip_u16(header, 26)?) + usize::from(zip_u16(header, 28)?);
    header.get(start..start.checked_add(size)?)
}

fn zip_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*bytes.get(at..)?.first_chunk()?))
}

fn zip_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*bytes.get(at..)?.first_chunk()?))
}

fn unpack_zip_lzma(payload: &[u8], size: u64, crc: u32) -> Option<Vec<u8>> {
    // ZIP method 14 prefixes the five standard LZMA properties with its version and length.
    if size > MAX_ENTRY_BYTES || zip_u16(payload, 2)? != 5 {
        return None;
    }
    let mut output = Vec::new();
    lzma_rs::lzma_decompress_with_options(
        &mut Cursor::new(payload.get(4..)?),
        &mut output,
        &Options {
            unpacked_size: UnpackedSize::UseProvided(Some(size)),
            memlimit: Some(MAX_ENTRY_BYTES as usize),
            ..Options::default()
        },
    )
    .ok()?;
    let mut checksum = flate2::Crc::new();
    checksum.update(&output);
    (output.len() as u64 == size && checksum.sum() == crc).then_some(output)
}

fn pak_entry_path(entry: &ZipEntry) -> Option<String> {
    if entry.path_is_unsafe() {
        return None;
    }
    let path = entry.path.to_ascii_lowercase().replace('\\', "/");
    (path.starts_with("materials/") && (path.ends_with(".vmt") || path.ends_with(".vtf")))
        .then_some(path)
}

fn material_path(value: &str, extension: &str) -> Option<String> {
    let path = value.trim().replace('\\', "/").to_ascii_lowercase();
    let path = path.trim_start_matches('/');
    let path = path.strip_prefix("materials/").unwrap_or(path);
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == ".." || part.contains([':', '\0']))
    {
        return None;
    }
    let path = path.strip_suffix(extension).unwrap_or(path);
    (!path.is_empty()).then(|| format!("materials/{path}{extension}"))
}

fn asset_roots(map_path: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(folder) = map_path.and_then(Path::parent) {
        let folder = if folder.as_os_str().is_empty() {
            Path::new(".")
        } else {
            folder
        };
        roots.push(folder.to_owned());
        if folder
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("maps"))
            && let Some(game) = folder.parent()
        {
            roots.push(game.to_owned());
            if let Some(install) = game.parent() {
                roots.push(install.join("hl2"));
            }
        }
    }
    if let Some(paths) = env::var_os("SURF_SOURCE_ASSETS") {
        for path in env::split_paths(&paths) {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("materials"))
            {
                roots.extend(path.parent().map(Path::to_owned));
            } else if path.is_file() {
                roots.push(path);
            } else {
                roots.extend([
                    path.clone(),
                    path.join("cstrike"),
                    path.join("hl2"),
                    path.join("csgo"),
                ]);
            }
        }
    }
    if map_path.is_some() {
        roots.extend(crate::source_steam::material_roots().iter().cloned());
    }
    let mut unique = Vec::new();
    for root in roots {
        if !unique.contains(&root) {
            unique.push(root);
        }
    }
    unique
}

fn read_bounded(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

fn archives(roots: &[PathBuf], limits: &Limits) -> Vec<Archive> {
    let mut archives = Vec::new();
    for root in roots {
        if root.is_file() {
            archives.extend(Archive::open(root.clone(), limits));
            continue;
        }
        let Ok(directory) = fs::read_dir(root) else {
            continue;
        };
        let mut paths: Vec<_> = directory
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with("_dir.vpk"))
            .map(|entry| entry.path())
            .collect();
        paths.sort();
        archives.extend(
            paths
                .into_iter()
                .filter_map(|path| Archive::open(path, limits)),
        );
    }
    archives
}

struct Archive {
    path: PathBuf,
    entries: HashMap<String, (VpkLocation, Vec<u8>)>,
}

impl Archive {
    fn open(path: PathBuf, limits: &Limits) -> Option<Self> {
        let bytes = read_bounded(&path, MAX_ENTRY_BYTES)?;
        let directory = vpk::parse(&bytes, limits).ok()?;
        let entries = directory
            .entries()
            .iter()
            .filter(|entry| entry.size() <= MAX_ENTRY_BYTES)
            .filter_map(|entry| {
                let path = entry.path.to_ascii_lowercase();
                (path.starts_with("materials/")
                    && (path.ends_with(".vmt") || path.ends_with(".vtf")))
                .then(|| (path, (entry.location, entry.preload.to_vec())))
            })
            .collect();
        Some(Self { path, entries })
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let (location, preload) = self.entries.get(path)?;
        let mut bytes = preload.clone();
        if location.is_empty() {
            return Some(bytes);
        }
        let (path, offset, length) = match *location {
            VpkLocation::InDirectory { offset, len } => (self.path.clone(), offset, len),
            VpkLocation::InArchive {
                archive,
                offset,
                len,
            } => {
                let name = vpk::sibling_archive_name(self.path.file_name()?.to_str()?, archive)?;
                (self.path.with_file_name(name), offset, len)
            }
        };
        let mut file = File::open(path).ok()?;
        file.seek(SeekFrom::Start(offset)).ok()?;
        file.take(length).read_to_end(&mut bytes).ok()?;
        (bytes.len() == preload.len() + length as usize).then_some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_zip_lzma_checks_size_and_crc() {
        let original = b"embedded material data";
        let mut stream = Vec::new();
        lzma_rs::lzma_compress(&mut Cursor::new(original), &mut stream).unwrap();
        let mut payload = vec![9, 20, 5, 0];
        payload.extend_from_slice(&stream[..5]);
        payload.extend_from_slice(&stream[13..]);

        let mut checksum = flate2::Crc::new();
        checksum.update(original);
        let size = original.len() as u64;
        let crc = checksum.sum();
        assert_eq!(
            unpack_zip_lzma(&payload, size, crc).as_deref(),
            Some(original.as_slice())
        );
        assert!(unpack_zip_lzma(&payload, size, crc ^ 1).is_none());
        assert!(unpack_zip_lzma(&payload, MAX_ENTRY_BYTES + 1, crc).is_none());
    }

    #[test]
    fn material_references_stay_inside_the_materials_tree() {
        assert_eq!(
            material_path("Materials\\Ramps\\Grid.VTF", ".vtf").as_deref(),
            Some("materials/ramps/grid.vtf"),
        );
        assert!(material_path("materials/../secret", ".vmt").is_none());
        assert!(material_path("C:/outside", ".vtf").is_none());
    }

    #[test]
    fn valve_uv_transform_scales_about_the_configured_center() {
        let transform =
            UvTransform::parse("center .5 .5 scale 2 3 rotate 0 translate 1 0").unwrap();
        assert_eq!(transform.apply([1.0, 1.0]), [2.5, 2.0]);
    }
}
