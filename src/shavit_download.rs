use crate::catalog::{MapMode, ShavitDownload};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::ser::{Formatter, PrettyFormatter};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

const MAX_BYTES: usize = 2 * 1024 * 1024;

#[derive(Deserialize)]
struct RawZone {
    #[serde(rename = "type")]
    kind: ZoneKind,
    point_a: [f64; 3],
    point_b: [f64; 3],
    #[serde(default, deserialize_with = "read_present")]
    form: Option<Value>,
    flags: Option<f64>,
    track: Option<f64>,
    index: Option<Value>,
    data: Option<Value>,
    target: Option<Value>,
    #[serde(default)]
    dest: Destination,
}

fn read_present<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[derive(Default)]
struct Destination(bool);

impl<'de> Deserialize<'de> for Destination {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(Self(true))
    }
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ZoneKind {
    Start,
    End,
    Stage,
    Checkpoint,
    #[serde(rename(deserialize = "autobhop"))]
    AllowBhop,
}

#[derive(Serialize)]
struct Sidecar<'a> {
    version: u8,
    bsp_sha256: &'a str,
    mode: MapMode,
    timer: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tier: Option<u8>,
    tracks: Vec<Track>,
    zones: Vec<Zone<'a>>,
    entity_output_fixes: [(); 0],
    #[serde(skip_serializing_if = "is_false")]
    source_gravity_outputs: bool,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    source_missing_filters: &'a [String],
    #[serde(skip_serializing_if = "is_false")]
    source_timed_triggers: bool,
    #[serde(skip_serializing_if = "is_false")]
    source_landmark_teleports: bool,
    #[serde(skip_serializing_if = "is_false")]
    source_player_filters: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Serialize)]
struct Track {
    id: u16,
    checkpoints_required: bool,
    checkpoints_ordered: bool,
    bhop_enabled: bool,
    limit_start_ground_speed: bool,
}

#[derive(Serialize)]
struct Zone<'a> {
    kind: ZoneKind,
    track: u16,
    points: [[f64; 2]; 4],
    bottom: f64,
    height: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hook_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hook_hammerid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,
}

#[derive(Default)]
struct TrackZones {
    starts: usize,
    ends: usize,
    start_targets: usize,
    checkpoints: BTreeSet<u16>,
    stages: BTreeSet<u16>,
}

pub(crate) fn normalize(
    raw: &[u8],
    bsp_sha256: &str,
    mode: MapMode,
    shavit: &ShavitDownload,
) -> Result<Vec<u8>, String> {
    if raw.len() > MAX_BYTES {
        return Err("Shavit zone data exceeds the timer limit".into());
    }
    // TextDecoder in the existing importer strips one UTF-8 byte order mark.
    let raw = raw.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(raw);
    let input: Vec<RawZone> = serde_json::from_slice(raw)
        .map_err(|error| format!("Invalid Shavit zone data: {error}"))?;
    let mut track_zones: BTreeMap<u16, TrackZones> = BTreeMap::new();
    let mut zones = Vec::with_capacity(input.len());
    for entry in input {
        let zone = entry.normalize(&shavit.spawn_targets)?;
        let track = track_zones.entry(zone.track).or_default();
        match zone.kind {
            ZoneKind::Start => {
                track.starts += 1;
                track.start_targets += usize::from(zone.target.is_some());
            }
            ZoneKind::End => track.ends += 1,
            ZoneKind::Stage => {
                track.stages.extend(zone.index);
            }
            ZoneKind::Checkpoint => {
                track.checkpoints.extend(zone.index);
            }
            ZoneKind::AllowBhop => {}
        }
        zones.push(zone);
    }
    if !track_zones.contains_key(&0) {
        return Err("Shavit timer has no main track".into());
    }

    let mut tracks = Vec::with_capacity(track_zones.len());
    for (id, track) in track_zones {
        if track.starts == 0 || track.ends == 0 {
            return Err(format!("Shavit track {id} needs both start and end zones"));
        }
        if track.start_targets > 1 {
            return Err(format!("Shavit track {id} needs a single start target"));
        }
        if !track.stages.is_empty() && !track.checkpoints.is_empty() {
            return Err("Mixed Shavit stage and checkpoint layouts are unsupported".into());
        }
        for (numbers, first) in [(&track.stages, 2), (&track.checkpoints, 1)] {
            if numbers
                .iter()
                .copied()
                .map(usize::from)
                .ne(first..first + numbers.len())
            {
                return Err(format!(
                    "Shavit track {id} has a missing stage or checkpoint"
                ));
            }
        }
        tracks.push(Track {
            id,
            checkpoints_required: !track.stages.is_empty(),
            checkpoints_ordered: true,
            bhop_enabled: mode == MapMode::Bhop,
            limit_start_ground_speed: false,
        });
    }

    let output = Sidecar {
        version: 1,
        bsp_sha256,
        mode,
        timer: "shavit",
        tier: shavit.tier,
        tracks,
        zones,
        entity_output_fixes: [],
        source_gravity_outputs: shavit.gravity_outputs,
        source_missing_filters: &shavit.missing_filters,
        source_timed_triggers: shavit.timed_triggers,
        source_landmark_teleports: shavit.landmark_teleports,
        source_player_filters: shavit.player_filters,
    };
    let mut bytes = Vec::new();
    output
        .serialize(&mut serde_json::Serializer::with_formatter(
            &mut bytes,
            ShavitFormatter::default(),
        ))
        .map_err(|error| format!("Could not normalize Shavit zones: {error}"))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_BYTES {
        return Err("Normalized Shavit timer exceeds the timer limit".into());
    }
    Ok(bytes)
}

impl RawZone {
    fn normalize<'a>(
        self,
        spawn_targets: &'a BTreeMap<String, String>,
    ) -> Result<Zone<'a>, String> {
        let form = match self.form.as_ref() {
            None => 0,
            Some(form) => whole(form.as_f64(), 1, "zone form")?,
        };
        let flags = whole(Some(self.flags.unwrap_or_default()), 3, "zone flags")?;
        if flags & 2 != 0 && form != 1 {
            return Err("Unsupported Shavit zone flags".into());
        }
        let track = whole(
            Some(self.track.unwrap_or_default()),
            u16::MAX.into(),
            "track",
        )? as u16;
        let min: [f64; 3] = std::array::from_fn(|i| self.point_a[i].min(self.point_b[i]));
        let max: [f64; 3] = std::array::from_fn(|i| self.point_a[i].max(self.point_b[i]));
        let height = max[2] - min[2];
        if (0..3).any(|i| min[i] >= max[i]) || !height.is_finite() {
            return Err("Shavit zone is empty or exceeds the coordinate range".into());
        }
        let first = match self.kind {
            ZoneKind::Stage => Some(2),
            ZoneKind::Checkpoint => Some(1),
            _ => None,
        };
        if self.dest.0 || (first.is_none() && self.data.as_ref().is_some_and(js_truthy)) {
            return Err("Unsupported Shavit zone destination or data".into());
        }
        let index = first
            .map(|first| {
                let value = self
                    .index
                    .as_ref()
                    .or(self.data.as_ref())
                    .and_then(Value::as_f64);
                let number = whole(value, u16::MAX.into(), "stage or checkpoint index")? as u16;
                if number < first {
                    return Err(format!(
                        "Shavit stage or checkpoint index must start at {first}"
                    ));
                }
                Ok(number)
            })
            .transpose()?;
        let (mut hook_target, mut hook_hammerid) = (None, None);
        if form == 1 {
            let hook = self
                .target
                .as_ref()
                .and_then(Value::as_str)
                .filter(|hook| !hook.is_empty())
                .ok_or("Invalid Shavit hook target")?;
            if flags & 2 != 0 {
                hook_hammerid = Some(whole(js_string_number(hook), u32::MAX, "Hammer ID")?);
            } else {
                hook_target = Some(hook.to_owned());
            }
        }
        let selector = match self.kind {
            ZoneKind::Start => Some(format!("start:{track}")),
            ZoneKind::Stage => index.map(|index| format!("stage:{track}:{index}")),
            _ => None,
        };
        let target = selector
            .as_ref()
            .and_then(|selector| spawn_targets.get(selector))
            .map(String::as_str);
        if target == Some("") {
            return Err("Invalid Shavit spawn target".into());
        }
        let [left, front, bottom] = min;
        let [right, back, _] = max;
        Ok(Zone {
            kind: self.kind,
            track,
            points: [[left, front], [right, front], [right, back], [left, back]],
            bottom,
            height,
            index,
            hook_target,
            hook_hammerid,
            target,
        })
    }
}

fn whole(number: Option<f64>, max: u32, name: &str) -> Result<u32, String> {
    number
        .filter(|n| *n >= 0.0 && *n <= f64::from(max) && n.fract() == 0.0)
        .map(|n| n as u32)
        .ok_or_else(|| format!("Invalid Shavit {name}"))
}

fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn js_string_number(raw: &str) -> Option<f64> {
    let value = raw.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if value.is_empty() {
        return Some(0.0);
    }
    for (prefixes, radix) in [(["0x", "0X"], 16), (["0o", "0O"], 8), (["0b", "0B"], 2)] {
        if let Some(digits) = prefixes
            .iter()
            .find_map(|prefix| value.strip_prefix(prefix))
        {
            return u32::from_str_radix(digits, radix).ok().map(f64::from);
        }
    }
    value.parse().ok()
}

#[derive(Default)]
struct ShavitFormatter(PrettyFormatter<'static>);

impl Formatter for ShavitFormatter {
    fn write_f64<W: ?Sized + Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        // The existing importer pins JSON.stringify output. Use serde's shortest
        // round-trip digits and match its ECMAScript notation and exponent.
        if value == 0.0 {
            return writer.write_all(b"0");
        }
        let mut rendered = Vec::with_capacity(32);
        self.0.write_f64(&mut rendered, value)?;
        let rendered = std::str::from_utf8(&rendered).expect("formatted float is ASCII");
        let (mantissa, exponent) = rendered.split_once('e').map_or((rendered, 0), |(m, e)| {
            (m, e.parse::<i32>().expect("formatted float exponent"))
        });
        let (sign, mantissa) = mantissa
            .strip_prefix('-')
            .map_or(("", mantissa), |m| ("-", m));
        let integer_digits = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
        let digits = mantissa.replace('.', "");
        let leading = digits.len() - digits.trim_start_matches('0').len();
        let digits = digits[leading..].trim_end_matches('0');
        let decimal = integer_digits + exponent - leading as i32;
        writer.write_all(sign.as_bytes())?;
        if decimal > 21 || decimal <= -6 {
            writer.write_all(&digits.as_bytes()[..1])?;
            if digits.len() > 1 {
                write!(writer, ".{}", &digits[1..])?;
            }
            return write!(writer, "e{:+}", decimal - 1);
        }
        if decimal <= 0 {
            writer.write_all(b"0.")?;
            for _ in decimal..0 {
                writer.write_all(b"0")?;
            }
            return writer.write_all(digits.as_bytes());
        }
        let decimal = decimal as usize;
        if decimal < digits.len() {
            return write!(writer, "{}.{}", &digits[..decimal], &digits[decimal..]);
        }
        writer.write_all(digits.as_bytes())?;
        for _ in digits.len()..decimal {
            writer.write_all(b"0")?;
        }
        Ok(())
    }

    fn begin_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_array(writer)
    }

    fn end_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_array(writer)
    }

    fn begin_array_value<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.begin_array_value(writer, first)
    }

    fn end_array_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_array_value(writer)
    }

    fn begin_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_object(writer)
    }

    fn end_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_object(writer)
    }

    fn begin_object_key<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.begin_object_key(writer, first)
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_object_value(writer)
    }

    fn end_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_object_value(writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> ShavitDownload {
        serde_json::from_value(serde_json::json!({ "sha256": "a".repeat(64) })).unwrap()
    }

    #[test]
    fn formats_normalized_shavit_in_importer_field_order() {
        let raw = br#"[
            {"type":"end","point_a":[32,32,-0.0],"point_b":[36,36,1e-7]},
            {"type":"start","point_a":[0,0,0.1],"point_b":[8,8,0.3]},
            {"type":"stage","data":2,"form":1,"flags":3,"target":"0x20",
             "point_a":[20,20,10],"point_b":[18,18,0]}
        ]"#;
        let mut shavit = options();
        shavit.tier = Some(3);
        shavit
            .spawn_targets
            .insert("stage:0:2".into(), "stage_2".into());
        let bytes = normalize(raw, &"a".repeat(64), MapMode::Surf, &shavit).unwrap();
        let output = String::from_utf8(bytes).unwrap();
        assert!(output.contains("\"bottom\": 0,\n      \"height\": 1e-7"));
        assert!(output.contains("\"height\": 0.19999999999999998"));
        assert!(output.contains(
            "\"height\": 10,\n      \"index\": 2,\n      \"hook_hammerid\": 32,\n      \"target\": \"stage_2\""
        ));
        assert!(output.ends_with("\n  ],\n  \"entity_output_fixes\": []\n}\n"));
        shavit.gravity_outputs = true;
        shavit.missing_filters.push("fix".into());
        shavit.timed_triggers = true;
        shavit.landmark_teleports = true;
        shavit.player_filters = true;
        let extended = normalize(raw, &"a".repeat(64), MapMode::Surf, &shavit).unwrap();
        assert!(String::from_utf8(extended).unwrap().ends_with(
            "\n  \"entity_output_fixes\": [],\n  \"source_gravity_outputs\": true,\n  \"source_missing_filters\": [\n    \"fix\"\n  ],\n  \"source_timed_triggers\": true,\n  \"source_landmark_teleports\": true,\n  \"source_player_filters\": true\n}\n"
        ));
        let json: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["tracks"][0]["checkpoints_required"], true);
        assert_eq!(json["tracks"][0]["bhop_enabled"], false);
        assert_eq!(json["tier"], 3);
    }

    #[test]
    fn rejects_shavit_routes_with_gaps_or_unsupported_fields() {
        let raw = br#"[
            {"type":"start","point_a":[0,0,0],"point_b":[8,8,8]},
            {"type":"end","point_a":[32,32,0],"point_b":[36,36,8]},
            {"type":"checkpoint","index":2,"point_a":[16,16,0],"point_b":[20,20,8]}
        ]"#;
        let shavit = options();
        let normalize_input = |raw: &[u8]| normalize(raw, &"a".repeat(64), MapMode::Bhop, &shavit);
        assert!(normalize_input(raw).unwrap_err().contains("missing"));
        let mut input: Vec<Value> = serde_json::from_slice(raw).unwrap();
        input[2]["index"] = Value::from(1);
        assert!(normalize_input(&serde_json::to_vec(&input).unwrap()).is_ok());
        input[2]["dest"] = Value::Null;
        assert!(
            normalize_input(&serde_json::to_vec(&input).unwrap())
                .unwrap_err()
                .contains("destination")
        );
        input[2].as_object_mut().unwrap().remove("dest");
        input[0]["form"] = Value::Null;
        assert!(
            normalize_input(&serde_json::to_vec(&input).unwrap())
                .unwrap_err()
                .contains("form")
        );
    }
}
