use std::{
    f32::consts::{PI, TAU},
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

use bevy_math::Vec3;
use serde::Deserialize;

use crate::{course::Course, movement::MoveInput};

pub const MAX_TRACK_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SAMPLES: usize = crate::replay::MAX_COMMANDS + 1;
const CROUCHED: u8 = 1;
const DISCONTINUITY: u8 = 2;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReferenceGhost {
    version: u8,
    map_id: String,
    map_version: String,
    geometry_sha256: String,
    #[serde(default)]
    bonus_pack_sha256: Option<String>,
    dt: f64,
    start_tick: f64,
    end_tick: f64,
    positions: Vec<f32>,
    yaws: Vec<f32>,
    flags: Vec<u8>,
    #[serde(default)]
    view: Option<ReferenceView>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReferenceView {
    pitches: Vec<f32>,
    eye_heights: Vec<f32>,
    velocities: Vec<f32>,
    splits: Vec<f64>,
    checkpoint_speeds: Vec<f32>,
    #[serde(default)]
    inputs: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReferenceGhostPose {
    pub position: Vec3,
    pub yaw: f32,
    pub crouched: bool,
}

#[derive(Clone, Copy)]
pub struct ReferenceReplayFrame {
    pub pose: ReferenceGhostPose,
    pub pitch: f32,
    pub eye_height: f32,
    pub velocity: Vec3,
    pub input: MoveInput,
}

impl ReferenceGhost {
    pub fn load(path: &Path) -> Result<Self, String> {
        let load = || {
            let file = File::open(path).map_err(|error| error.to_string())?;
            let mut bytes = Vec::new();
            BufReader::new(file)
                .take(MAX_TRACK_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            Self::decode(&bytes)
        };
        load().map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_TRACK_BYTES {
            return Err("Reference ghost file exceeds the size limit".into());
        }
        let track: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let count = track.yaws.len();
        let bonus = bonus_identity(&track.map_version)?;
        let valid_identity = !track.map_id.is_empty()
            && track.map_id.len() <= 96
            && track
                .map_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            && !track.map_version.is_empty()
            && track.map_version.len() <= 128
            && valid_hash(&track.geometry_sha256)
            && match bonus {
                Some(_) => track.bonus_pack_sha256.as_deref().is_some_and(valid_hash),
                None => track.bonus_pack_sha256.is_none(),
            };
        if track.version != 1 || !valid_identity {
            return Err("Unknown reference ghost format or map identity".into());
        }
        if !(2..=MAX_SAMPLES).contains(&count)
            || track.positions.len() != count * 3
            || track.flags.len() != count
            || !track
                .flags
                .iter()
                .all(|flag| flag & !(CROUCHED | DISCONTINUITY) == 0)
            || !track
                .positions
                .iter()
                .all(|component| component.abs() <= 100_000.0)
            || !track.yaws.iter().all(|yaw| yaw.abs() <= 720.0)
            || !track.dt.is_finite()
            || track.dt <= 0.0
            || track.dt > 0.1
            || !track.start_tick.is_finite()
            || !track.end_tick.is_finite()
            || track.start_tick < 0.0
            || track.end_tick <= track.start_tick
            || track.end_tick > (count - 1) as f64
        {
            return Err("Invalid reference ghost trajectory".into());
        }
        if let Some(view) = &track.view
            && (view.pitches.len() != count
                || view.eye_heights.len() != count
                || view.velocities.len() != count * 3
                || view.inputs.as_ref().is_some_and(|inputs| {
                    inputs.len() != count || inputs.iter().any(|input| *input > 63)
                })
                || !view.pitches.iter().all(|pitch| pitch.abs() <= 90.0)
                || !view
                    .eye_heights
                    .iter()
                    .all(|height| (0.0..=128.0).contains(height))
                || !view.velocities.iter().all(|speed| speed.abs() <= 100_000.0)
                || !view
                    .splits
                    .iter()
                    .all(|split| (0.0..=track.duration()).contains(split))
                || view.splits.windows(2).any(|pair| pair[0] > pair[1])
                || view.checkpoint_speeds.len() != view.splits.len()
                || !view
                    .checkpoint_speeds
                    .iter()
                    .all(|speed| (0.0..=100_000.0).contains(speed)))
        {
            return Err("Invalid reference replay camera or timing data".into());
        }
        Ok(track)
    }

    pub fn map_id(&self) -> &str {
        &self.map_id
    }

    pub fn bonus_in_version(map_version: &str) -> Result<Option<u16>, &'static str> {
        Ok(bonus_identity(map_version)?.map(|(_, number, _)| number))
    }

    pub fn bonus(&self) -> Option<u16> {
        Self::bonus_in_version(&self.map_version).expect("validated reference replay revision")
    }

    pub fn matches_course(&self, course: &Course) -> bool {
        if self.map_id != course.name
            || course.catalog_revision.as_deref() != Some(self.geometry_sha256.as_str())
        {
            return false;
        }
        match bonus_identity(&self.map_version).expect("validated reference replay revision") {
            Some((main, number, revision)) => {
                course.map_triggers.source_version.as_deref() == Some(main)
                    && course.map_triggers.bonus_pack_sha256 == self.bonus_pack_sha256
                    && course
                        .bonus(number)
                        .is_some_and(|route| route.revision.parse::<u64>().ok() == Some(revision))
            }
            None => {
                course.map_triggers.source_version.as_deref() == Some(self.map_version.as_str())
            }
        }
    }

    pub fn duration(&self) -> f64 {
        (self.end_tick - self.start_tick) * self.dt
    }

    pub fn sample_count(&self) -> usize {
        self.yaws.len()
    }

    pub fn tick_seconds(&self) -> f64 {
        self.dt
    }

    pub fn elapsed_at(&self, tick: usize) -> f64 {
        ((tick as f64 - self.start_tick) * self.dt).clamp(0.0, self.duration())
    }

    pub fn started_at(&self, tick: usize) -> bool {
        tick as f64 >= self.start_tick
    }

    pub fn recorded_splits(&self) -> Option<(&[f64], &[f32])> {
        let view = self.view.as_ref()?;
        Some((&view.splits, &view.checkpoint_speeds))
    }

    pub fn replay_frame(&self, index: usize) -> Option<ReferenceReplayFrame> {
        let view = self.view.as_ref()?;
        let pitch = -view.pitches.get(index)?.to_radians();
        let pose = self.frame(index);
        let input = view.inputs.as_ref().map_or(0, |inputs| inputs[index]);
        let bit = |mask| i8::from(input & mask != 0);
        Some(ReferenceReplayFrame {
            pose,
            pitch,
            eye_height: view.eye_heights[index],
            velocity: source_vector(&view.velocities, index),
            input: MoveInput {
                forward: f32::from(bit(1) - bit(2)),
                strafe: f32::from(bit(4) - bit(8)),
                yaw: pose.yaw,
                jump: bit(16) != 0,
                crouch: bit(32) != 0,
            },
        })
    }

    pub fn sample(&self, elapsed: f64) -> Option<ReferenceGhostPose> {
        if !elapsed.is_finite() {
            return None;
        }
        let tick = (self.start_tick + elapsed.max(0.0) / self.dt).min(self.end_tick);
        let from = tick.floor() as usize;
        let alpha = (tick - from as f64) as f32;
        if alpha == 0.0 {
            return Some(self.frame(from));
        }
        let next = from + 1;
        if self.flags[next] & DISCONTINUITY != 0 {
            return Some(self.frame(next));
        }
        let before = self.frame(from);
        let after = self.frame(next);
        let turn = (after.yaw - before.yaw + PI).rem_euclid(TAU) - PI;
        Some(ReferenceGhostPose {
            position: before.position.lerp(after.position, alpha),
            yaw: (before.yaw + turn * alpha).rem_euclid(TAU),
            crouched: if alpha < 0.5 {
                before.crouched
            } else {
                after.crouched
            },
        })
    }

    fn frame(&self, index: usize) -> ReferenceGhostPose {
        ReferenceGhostPose {
            position: source_vector(&self.positions, index),
            yaw: (self.yaws[index] - 90.0).to_radians().rem_euclid(TAU),
            crouched: self.flags[index] & CROUCHED != 0,
        }
    }
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn bonus_identity(map_version: &str) -> Result<Option<(&str, u16, u64)>, &'static str> {
    let Some((main, suffix)) = map_version.rsplit_once("/bonus-") else {
        return Ok(None);
    };
    let error = "Invalid reference replay bonus revision";
    let canonical_digits = |value: &str| {
        !value.is_empty()
            && !value.starts_with('0')
            && value.bytes().all(|byte| byte.is_ascii_digit())
    };
    let (number, revision) = if let Some((number, revision)) = suffix.split_once(".r") {
        if !canonical_digits(revision) {
            return Err(error);
        }
        let revision: u64 = revision.parse().map_err(|_| error)?;
        if revision <= 1 {
            return Err(error);
        }
        (number, revision)
    } else {
        (suffix, 1)
    };
    if main.is_empty() || !canonical_digits(number) {
        return Err(error);
    }
    Ok(Some((main, number.parse().map_err(|_| error)?, revision)))
}

fn source_vector(values: &[f32], index: usize) -> Vec3 {
    let offset = index * 3;
    Vec3::new(values[offset], values[offset + 2], -values[offset + 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bonus_tracks_require_the_exact_catalog_pack_and_route_revision() {
        use crate::course::{BonusRoute, PracticeSpawn};

        let geometry_hash = "a".repeat(64);
        let pack_hash = "b".repeat(64);
        let mut course = Course::practice();
        course.name = "surf_test".into();
        course.catalog_revision = Some(geometry_hash.clone());
        course.map_triggers.source_version = Some("test-v1".into());
        course.map_triggers.bonus_pack_sha256 = Some(pack_hash.clone());
        course.bonus_routes.push(BonusRoute {
            number: 2,
            name: "Bonus 2".into(),
            spawn: PracticeSpawn {
                name: "Bonus 2".into(),
                position: Vec3::ZERO,
                yaw: 0.0,
                pitch: 0.0,
                source_yaw_degrees: None,
            },
            zones: Vec::new(),
            revision: "3".into(),
        });
        let mut raw = serde_json::json!({
            "version": 1, "mapId": "surf_test", "mapVersion": "test-v1/bonus-2.r3",
            "geometrySha256": geometry_hash, "bonusPackSha256": pack_hash,
            "dt": 0.015, "startTick": 0.0, "endTick": 1.0,
            "positions": [0.0, 0.0, 0.0, 10.0, 20.0, 30.0], "yaws": [90.0, 90.0], "flags": [0, 0],
        });
        let decode =
            |raw: &serde_json::Value| ReferenceGhost::decode(&serde_json::to_vec(raw).unwrap());
        let bonus = decode(&raw).unwrap();
        assert_eq!(bonus.bonus(), Some(2));
        assert!(bonus.matches_course(&course));
        for version in ["test-v1/bonus-2", "test-v1/bonus-1.r3", "other/bonus-2.r3"] {
            raw["mapVersion"] = version.into();
            assert!(!decode(&raw).unwrap().matches_course(&course));
        }
        for version in [
            "test-v1/bonus-2.r1",
            "test-v1/bonus-02.r3",
            "test-v1/bonus-65536",
        ] {
            raw["mapVersion"] = version.into();
            assert!(decode(&raw).is_err());
        }
        raw["mapVersion"] = "test-v1/bonus-2.r3".into();
        raw["bonusPackSha256"] = "c".repeat(64).into();
        assert!(!decode(&raw).unwrap().matches_course(&course));
        raw.as_object_mut().unwrap().remove("bonusPackSha256");
        assert!(decode(&raw).is_err());
        raw["mapVersion"] = "test-v1".into();
        assert!(decode(&raw).unwrap().matches_course(&course));
    }

    #[test]
    fn source_axes_and_discontinuities_are_preserved() {
        let raw = serde_json::json!({
            "version": 1, "mapId": "surf_test", "mapVersion": "test-v1",
            "geometrySha256": "a".repeat(64), "dt": 0.015, "startTick": 0.0, "endTick": 2.0,
            "positions": [0.0, 0.0, 0.0, 10.0, 20.0, 30.0, 200.0, 300.0, 400.0],
            "yaws": [80.0, 100.0, 90.0], "flags": [0, 1, 2],
        });
        let ghost = ReferenceGhost::decode(&serde_json::to_vec(&raw).unwrap()).unwrap();
        let middle = ghost.sample(0.0075).unwrap();
        assert_eq!(middle.position, Vec3::new(5.0, 15.0, -10.0));
        assert!(middle.yaw.abs().min((middle.yaw - TAU).abs()) < 1e-5);
        assert!(middle.crouched);
        assert_eq!(
            ghost.sample(0.02).unwrap().position,
            Vec3::new(200.0, 400.0, -300.0)
        );
        assert!((ghost.duration() - 0.03).abs() < 1e-8);
    }

    #[test]
    fn first_person_tracks_keep_fractional_timing_and_reject_missing_frames() {
        let mut raw = serde_json::json!({
            "version": 1, "mapId": "surf_test", "mapVersion": "test-v1",
            "geometrySha256": "a".repeat(64), "dt": 0.015, "startTick": 0.25, "endTick": 1.75,
            "positions": [0.0, 0.0, 0.0, 10.0, 20.0, 30.0, 40.0, 50.0, 60.0],
            "yaws": [90.0, 90.0, 90.0], "flags": [0, 1, 1],
            "view": {
                "pitches": [0.0, 45.0, 45.0], "eyeHeights": [64.0, 56.0, 46.0],
                "velocities": [0.0, 0.0, 0.0, 10.0, 20.0, 30.0, 0.0, 0.0, 0.0],
                "splits": [0.01125], "checkpointSpeeds": [37.0],
            },
        });
        let replay = ReferenceGhost::decode(&serde_json::to_vec(&raw).unwrap()).unwrap();
        assert!(!replay.started_at(0));
        assert!((replay.elapsed_at(1) - 0.01125).abs() < 1e-12);
        assert!((replay.elapsed_at(2) - 0.0225).abs() < 1e-12);
        let frame = replay.replay_frame(1).unwrap();
        assert_eq!(frame.eye_height, 56.0);
        assert_eq!(frame.pitch, -std::f32::consts::FRAC_PI_4);
        assert_eq!(frame.velocity, Vec3::new(10.0, 30.0, -20.0));
        assert_eq!(
            replay.recorded_splits(),
            Some((&[0.01125][..], &[37.0][..]))
        );
        raw["view"]["pitches"] = serde_json::json!([0.0]);
        assert!(ReferenceGhost::decode(&serde_json::to_vec(&raw).unwrap()).is_err());
    }
}
