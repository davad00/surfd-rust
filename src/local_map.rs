//! Routes local map files to the loader for their on-disk format.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use flate2::read::MultiGzDecoder;

use crate::catalog::MapMode;
use crate::course::{Course, ZoneKind};
use crate::{goldsrc_bsp, map_materials, quake_bsp, quake3_bsp, source_bsp};

pub fn map_name(path: &Path) -> Option<&str> {
    if is_pk3(path) {
        let name = path.file_stem()?.to_str()?;
        if name
            .get(..3)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("pak"))
            && name
                .get(3..)
                .is_some_and(|suffix| suffix.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return None;
        }
        Some(name)
    } else {
        source_bsp::map_name(path)
    }
}

pub fn default_mode(path: &Path, name: &str) -> MapMode {
    if is_pk3(path) {
        MapMode::Bhop
    } else {
        MapMode::from_map_name(name)
    }
}

pub fn load(path: &Path) -> Result<Course, String> {
    load_with_bc(path, false)
}

pub fn load_with_bc(path: &Path, use_bc: bool) -> Result<Course, String> {
    let mut course = match format(path)? {
        Format::Source => return source_bsp::load_with_bc(path, use_bc),
        Format::Quake => quake_bsp::load(path),
        Format::GoldSrc => goldsrc_bsp::load(path),
        Format::Quake3 => quake3_bsp::load(path),
    }?;
    if let Some(mode) = source_bsp::map_metadata(path).mode {
        course.map_triggers.source_mode = Some(mode);
    }
    map_materials::fill_missing_surfaces(&mut course);
    Ok(course)
}

pub fn has_timer(path: &Path, metadata: &source_bsp::MapMetadata) -> bool {
    match format(path) {
        Ok(Format::Source) => metadata
            .timer_bsp_sha256
            .as_deref()
            .is_some_and(|checksum| source_bsp::matches_timer_bsp(path, checksum)),
        Ok(_) => load(path).is_ok_and(|course| has_start_and_finish(&course)),
        Err(_) => false,
    }
}

pub fn has_start_and_finish(course: &Course) -> bool {
    course
        .zones
        .iter()
        .any(|zone| matches!(zone.kind, ZoneKind::Start))
        && course
            .zones
            .iter()
            .any(|zone| matches!(zone.kind, ZoneKind::Finish))
}

enum Format {
    Source,
    Quake,
    GoldSrc,
    Quake3,
}

fn format(path: &Path) -> Result<Format, String> {
    let mut header = [0_u8; 8];
    File::open(path)
        .and_then(|mut file| {
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"))
            {
                MultiGzDecoder::new(file).read_exact(&mut header)
            } else {
                file.read_exact(&mut header)
            }
        })
        .map_err(|error| format!("Could not read map header {}: {error}", path.display()))?;

    let magic: [u8; 4] = header[..4].try_into().unwrap();
    let version = u32::from_le_bytes(header[4..].try_into().unwrap());
    match &magic {
        b"VBSP" if matches!(version, 19 | 20 | 21 | 25) => Ok(Format::Source),
        b"VBSP" => Err(format!("Unsupported Source BSP version {version}")),
        b"IBSP" if version == 46 => Ok(Format::Quake3),
        b"IBSP" => Err(format!("Unsupported IBSP version {version}")),
        b"PK\x03\x04" if is_pk3(path) => Ok(Format::Quake3),
        _ => match u32::from_le_bytes(magic) {
            29 => Ok(Format::Quake),
            30 => Ok(Format::GoldSrc),
            _ => Err("Unsupported local map format".into()),
        },
    }
}

fn is_pk3(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pk3"))
}
