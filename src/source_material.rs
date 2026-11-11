//! Resolves Source materials from the map pakfile, loose files and game VPKs.

use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use vformats::bsp::{ZipEntry, ZipReader};
use vformats::vpk::{self, VpkLocation};
use vformats::{Limits, vmt, vtf};

use crate::course::{CourseTexture, TextureAlpha};

const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DECODED_BYTES: usize = 256 * 1024 * 1024;
const MAX_PATCH_DEPTH: u8 = 8;

pub(crate) struct Materials<'a> {
    pak: Option<ZipReader<'a>>,
    pak_index: HashMap<String, usize>,
    roots: Vec<PathBuf>,
    archives: Option<Vec<Archive>>,
    limits: Limits,
    resolved: HashMap<String, Option<Resolved>>,
    decoded: HashMap<(String, Alpha), usize>,
    textures: Vec<CourseTexture>,
    decoded_bytes: usize,
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
    pub fn new(pak_bytes: &'a [u8], map_path: Option<&Path>) -> Self {
        let pak = ZipReader::parse(pak_bytes).ok();
        let pak_index = pak
            .as_ref()
            .into_iter()
            .flat_map(ZipReader::entries)
            .enumerate()
            .filter_map(|(index, entry)| pak_entry_path(entry).map(|path| (path, index)))
            .collect();
        Self {
            pak,
            pak_index,
            roots: asset_roots(map_path),
            archives: None,
            limits: Limits {
                max_entry_bytes: MAX_ENTRY_BYTES,
                ..Limits::default()
            },
            resolved: HashMap::new(),
            decoded: HashMap::new(),
            textures: Vec::new(),
            decoded_bytes: 0,
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

    fn resolve_material(&mut self, name: &str) -> Option<Resolved> {
        let path = material_path(name, ".vmt")?;
        let definition = self.definition(&path, MAX_PATCH_DEPTH).unwrap_or_default();
        let alpha = definition.alpha();
        let path = definition.base.or_else(|| material_path(name, ".vtf"))?;
        let key = (path, alpha);
        let index = if let Some(index) = self.decoded.get(&key) {
            *index
        } else {
            let bytes = self.read(&key.0)?;
            let texture = vtf::parse(&bytes, &self.limits).ok()?;
            let output_bytes = texture.width() as usize * texture.height() as usize * 4;
            if self.decoded_bytes.saturating_add(output_bytes) > MAX_DECODED_BYTES {
                return None;
            }
            let image = texture.decode_rgba(0, 0, 0, &self.limits).ok()?;
            self.decoded_bytes += image.rgba.len();
            let index = self.textures.len();
            self.textures.push(CourseTexture {
                bc: None,
                name: key.0.clone(),
                width: image.width,
                height: image.height,
                rgba: image.rgba,
                alpha: alpha.into(),
            });
            self.decoded.insert(key, index);
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
            pak.entry_bytes(pak.entries().get(index)?, &self.limits)
                .ok()
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
