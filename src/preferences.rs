use std::{
    collections::BTreeMap,
    fmt::{self, Write},
};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::cosmetics::{AvatarSkin, KnifeFinish};

const SOURCE_ASPECT: f32 = 4.0 / 3.0;
const SOURCE_MOUSE_YAW: f64 = 0.022;
pub const MAX_BINDINGS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Crouch,
    Restart,
    RestartStage,
    SavePosition,
    LoadPosition,
    TogglePractice,
    ToggleCamera,
    Diagnostics,
    TurnLeft,
    TurnRight,
    CycleYaw,
    AlternateYaw,
}

impl ControlAction {
    pub const MOVEMENT: [Self; 13] = [
        Self::Forward,
        Self::Back,
        Self::Left,
        Self::Right,
        Self::Jump,
        Self::Crouch,
        Self::Restart,
        Self::RestartStage,
        Self::SavePosition,
        Self::LoadPosition,
        Self::TogglePractice,
        Self::ToggleCamera,
        Self::Diagnostics,
    ];
    pub const TURNS: [Self; 4] = [
        Self::TurnLeft,
        Self::TurnRight,
        Self::CycleYaw,
        Self::AlternateYaw,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Forward => "Move forward",
            Self::Back => "Move back",
            Self::Left => "Move left",
            Self::Right => "Move right",
            Self::Jump => "Jump",
            Self::Crouch => "Crouch",
            Self::Restart => "Restart run",
            Self::RestartStage => "Restart current stage",
            Self::SavePosition => "Save position",
            Self::LoadPosition => "Load position",
            Self::TogglePractice => "Toggle practice",
            Self::ToggleCamera => "Toggle player camera",
            Self::Diagnostics => "Movement diagnostics",
            Self::TurnLeft => "Turn left",
            Self::TurnRight => "Turn right",
            Self::CycleYaw => "Cycle yaw speed",
            Self::AlternateYaw => "Hold for alternate yaw speed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    Key(String),
    Mouse(MouseBinding),
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseBinding {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    Other(u16),
}

impl From<MouseButton> for MouseBinding {
    fn from(value: MouseButton) -> Self {
        match value {
            MouseButton::Left => Self::Left,
            MouseButton::Right => Self::Right,
            MouseButton::Middle => Self::Middle,
            MouseButton::Back => Self::Back,
            MouseButton::Forward => Self::Forward,
            MouseButton::Other(index) => Self::Other(index),
        }
    }
}

impl From<MouseBinding> for MouseButton {
    fn from(value: MouseBinding) -> Self {
        match value {
            MouseBinding::Left => Self::Left,
            MouseBinding::Right => Self::Right,
            MouseBinding::Middle => Self::Middle,
            MouseBinding::Back => Self::Back,
            MouseBinding::Forward => Self::Forward,
            MouseBinding::Other(index) => Self::Other(index),
        }
    }
}

impl Binding {
    pub fn key(key: KeyCode) -> Self {
        Self::Key(format!("{key:?}"))
    }

    pub fn wheel(scroll: Vec2) -> Option<Self> {
        if scroll.y > 0.0 {
            Some(Self::WheelUp)
        } else if scroll.y < 0.0 {
            Some(Self::WheelDown)
        } else {
            None
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Key(key) => match key.as_str() {
                "ControlLeft" => "Left Ctrl".into(),
                "ControlRight" => "Right Ctrl".into(),
                "ShiftLeft" => "Left Shift".into(),
                "ShiftRight" => "Right Shift".into(),
                "AltLeft" => "Left Alt".into(),
                "AltRight" => "Right Alt".into(),
                _ => key
                    .strip_prefix("Key")
                    .or_else(|| key.strip_prefix("Digit"))
                    .unwrap_or(key)
                    .into(),
            },
            Self::Mouse(button) => match button {
                MouseBinding::Left => "Mouse 1".into(),
                MouseBinding::Right => "Mouse 2".into(),
                MouseBinding::Middle => "Mouse 3".into(),
                MouseBinding::Back => "Mouse 4".into(),
                MouseBinding::Forward => "Mouse 5".into(),
                MouseBinding::Other(index) => format!("Mouse {index}"),
            },
            Self::WheelUp => "Wheel up".into(),
            Self::WheelDown => "Wheel down".into(),
        }
    }
}

fn matches_key_name(key: KeyCode, name: &str) -> bool {
    struct KeyName<'a>(&'a str);

    impl Write for KeyName<'_> {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            self.0 = self.0.strip_prefix(text).ok_or(fmt::Error)?;
            Ok(())
        }
    }

    // Existing settings use Bevy's Debug spelling, including unidentified platform keys.
    let mut expected = KeyName(name);
    write!(expected, "{key:?}").is_ok() && expected.0.is_empty()
}

fn default_bindings() -> BTreeMap<ControlAction, Vec<Binding>> {
    use ControlAction::*;
    use KeyCode::*;

    BTreeMap::from([
        (Forward, vec![Binding::key(KeyW)]),
        (Back, vec![Binding::key(KeyS)]),
        (Left, vec![Binding::key(KeyA)]),
        (Right, vec![Binding::key(KeyD)]),
        (Jump, vec![Binding::key(Space), Binding::WheelDown]),
        (
            Crouch,
            vec![
                Binding::key(ControlLeft),
                Binding::key(ControlRight),
                Binding::key(KeyC),
            ],
        ),
        (Restart, vec![Binding::key(KeyR)]),
        (SavePosition, vec![Binding::key(F5)]),
        (LoadPosition, vec![Binding::key(F9)]),
        (TogglePractice, vec![Binding::key(KeyP)]),
        (ToggleCamera, vec![Binding::key(KeyV)]),
        (Diagnostics, vec![Binding::key(F3)]),
    ])
}

#[derive(Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    #[serde(default)]
    version: u8,
    pub sensitivity: f64,
    pub fov: f32,
    pub invert_mouse: bool,
    pub smooth_camera: bool,
    pub auto_bhop: bool,
    pub unrestricted_start: bool,
    pub show_keys: bool,
    pub show_crosshair: bool,
    pub vsync: bool,
    pub fullscreen: bool,
    pub menu_motion: bool,
    pub environmental_audio: bool,
    pub environmental_volume: f32,
    pub hud_scale: f32,
    pub center_speed: bool,
    pub show_sync: bool,
    pub compare_splits: bool,
    pub compare_speed: bool,
    pub colorblind: bool,
    pub diagnostics: bool,
    pub crosshair: CrosshairStyle,
    pub avatar_skin: AvatarSkin,
    pub knife_finish: KnifeFinish,
    pub turns_enabled: bool,
    pub yaw_speeds: Vec<f32>,
    pub yaw_index: usize,
    pub alternate_yaw: f32,
    bindings: BTreeMap<ControlAction, Vec<Binding>>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 2,
            sensitivity: 2.5,
            fov: 90.0,
            invert_mouse: false,
            smooth_camera: false,
            auto_bhop: true,
            unrestricted_start: false,
            show_keys: true,
            show_crosshair: true,
            vsync: true,
            fullscreen: false,
            menu_motion: true,
            environmental_audio: true,
            environmental_volume: 0.14,
            hud_scale: 1.0,
            center_speed: false,
            show_sync: false,
            compare_splits: true,
            compare_speed: false,
            colorblind: false,
            diagnostics: false,
            crosshair: CrosshairStyle::default(),
            avatar_skin: AvatarSkin::default(),
            knife_finish: KnifeFinish::default(),
            turns_enabled: false,
            yaw_speeds: vec![210.0],
            yaw_index: 0,
            alternate_yaw: 140.0,
            bindings: default_bindings(),
        }
    }
}

impl Settings {
    pub fn read(bytes: &[u8]) -> Self {
        let Ok(mut settings) = serde_json::from_slice::<Self>(bytes) else {
            return Self::default();
        };
        if settings.version == 0 {
            // Preserve the effective camera sensitivity and vertical angle from old installs.
            settings.sensitivity *= 0.0022 / SOURCE_MOUSE_YAW.to_radians();
            settings.fov = (2.0 * ((settings.fov.to_radians() / 2.0).tan() * SOURCE_ASPECT).atan())
                .to_degrees();
        }
        if settings.version < 2 {
            settings
                .bindings
                .entry(ControlAction::ToggleCamera)
                .or_insert_with(|| vec![Binding::key(KeyCode::KeyV)]);
        }
        settings.version = 2;
        settings.sensitivity = settings.sensitivity.clamp(0.0, 20.0);
        settings.fov = settings.fov.clamp(50.0, 130.0);
        settings.hud_scale = settings.hud_scale.clamp(0.7, 1.8);
        settings.environmental_volume = settings.environmental_volume.clamp(0.0, 1.0);
        settings.yaw_speeds.retain(|rate| valid_yaw_rate(*rate));
        settings.yaw_speeds.truncate(8);
        if settings.yaw_speeds.is_empty() {
            settings.yaw_speeds.push(210.0);
        }
        settings.yaw_index = settings.yaw_index.min(settings.yaw_speeds.len() - 1);
        settings.alternate_yaw = settings.alternate_yaw.clamp(-100_000.0, 100_000.0);
        settings.crosshair.normalize();
        for bindings in settings.bindings.values_mut() {
            bindings.truncate(MAX_BINDINGS);
        }
        settings
    }

    pub fn mouse_degrees_per_count(&self) -> f64 {
        SOURCE_MOUSE_YAW * self.sensitivity
    }

    pub fn vertical_fov_radians(&self) -> f32 {
        2.0 * ((self.fov.to_radians() / 2.0).tan() / SOURCE_ASPECT).atan()
    }

    /// Keyboard turn speed in degrees per second, using Source's signed yaw convention.
    pub fn yaw_rate(&self, alternate: bool) -> f64 {
        if !self.turns_enabled {
            return 0.0;
        }
        if alternate {
            f64::from(self.alternate_yaw)
        } else {
            f64::from(self.yaw_speeds[self.yaw_index])
        }
    }

    pub fn cycle_yaw_rate(&mut self) {
        if self.turns_enabled {
            self.yaw_index = (self.yaw_index + 1) % self.yaw_speeds.len();
        }
    }

    pub fn active(
        &self,
        action: ControlAction,
        keys: &ButtonInput<KeyCode>,
        mouse: &ButtonInput<MouseButton>,
        scroll: Vec2,
    ) -> bool {
        self.bindings(action).iter().any(|binding| match binding {
            Binding::Key(key) => keys
                .get_pressed()
                .any(|&pressed| matches_key_name(pressed, key)),
            Binding::Mouse(button) => mouse.pressed((*button).into()),
            Binding::WheelUp => scroll.y > 0.0,
            Binding::WheelDown => scroll.y < 0.0,
        })
    }

    pub fn just_pressed(
        &self,
        action: ControlAction,
        keys: &ButtonInput<KeyCode>,
        mouse: &ButtonInput<MouseButton>,
        scroll: Vec2,
    ) -> bool {
        self.bindings(action).iter().any(|binding| match binding {
            Binding::Key(key) => keys
                .get_just_pressed()
                .any(|&pressed| matches_key_name(pressed, key)),
            Binding::Mouse(button) => mouse.just_pressed((*button).into()),
            Binding::WheelUp => scroll.y > 0.0,
            Binding::WheelDown => scroll.y < 0.0,
        })
    }

    pub fn bindings(&self, action: ControlAction) -> &[Binding] {
        self.bindings.get(&action).map_or(&[], Vec::as_slice)
    }

    pub fn primary_binding(&self, action: ControlAction) -> String {
        self.bindings(action)
            .first()
            .map_or_else(|| "Unbound".into(), Binding::label)
    }

    pub fn set_binding(&mut self, action: ControlAction, slot: Option<usize>, value: Binding) {
        let bindings = self.bindings.entry(action).or_default();
        if bindings.contains(&value) {
            return;
        }
        if let Some(index) = slot.filter(|index| *index < bindings.len()) {
            bindings[index] = value;
        } else if bindings.len() < MAX_BINDINGS {
            bindings.push(value);
        }
    }

    pub fn clear_binding(&mut self, action: ControlAction, slot: usize) {
        if let Some(bindings) = self.bindings.get_mut(&action)
            && slot < bindings.len()
        {
            bindings.remove(slot);
        }
    }

    pub fn reset_bindings(&mut self) {
        self.bindings = default_bindings();
    }
}

pub fn valid_yaw_rate(rate: f32) -> bool {
    rate.is_finite() && rate.abs() <= 100_000.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct CrosshairStyle {
    pub length: u8,
    pub thickness: u8,
    pub gap: u8,
    pub dot: u8,
    pub circle: u8,
    pub circle_width: u8,
    pub outline: u8,
    pub t_shape: bool,
    pub rgb: [u8; 3],
    pub opacity: u8,
}

impl Default for CrosshairStyle {
    fn default() -> Self {
        Self {
            length: 5,
            thickness: 1,
            gap: 0,
            dot: 0,
            circle: 0,
            circle_width: 1,
            outline: 0,
            t_shape: false,
            rgb: [132, 211, 72],
            opacity: 100,
        }
    }
}

#[derive(Clone, Copy)]
pub enum CrosshairPart {
    Length,
    Thickness,
    Gap,
    Dot,
    Circle,
    CircleWidth,
    Outline,
    Opacity,
}

impl CrosshairPart {
    pub const ALL: [Self; 8] = [
        Self::Length,
        Self::Thickness,
        Self::Gap,
        Self::Dot,
        Self::Circle,
        Self::CircleWidth,
        Self::Outline,
        Self::Opacity,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Length => "Line length",
            Self::Thickness => "Line thickness",
            Self::Gap => "Gap",
            Self::Dot => "Dot size",
            Self::Circle => "Circle radius",
            Self::CircleWidth => "Circle width",
            Self::Outline => "Outline",
            Self::Opacity => "Opacity",
        }
    }

    fn bounds(self) -> (u8, u8) {
        match self {
            Self::Length => (0, 20),
            Self::Thickness => (1, 10),
            Self::Gap => (0, 12),
            Self::Dot => (0, 10),
            Self::Circle => (0, 24),
            Self::CircleWidth => (1, 6),
            Self::Outline => (0, 3),
            Self::Opacity => (10, 100),
        }
    }
}

impl CrosshairStyle {
    pub fn preset(index: usize) -> Self {
        let empty = Self {
            length: 0,
            rgb: [255; 3],
            ..Self::default()
        };
        match index {
            1 => Self {
                length: 6,
                gap: 4,
                rgb: [50, 250, 50],
                opacity: 80,
                ..empty
            },
            2 => Self {
                dot: 3,
                outline: 1,
                ..empty
            },
            3 => Self {
                length: 4,
                gap: 3,
                dot: 1,
                ..Self::default()
            },
            4 => Self {
                length: 3,
                gap: 1,
                outline: 1,
                rgb: [0, 255, 255],
                ..empty
            },
            5 => Self {
                gap: 2,
                t_shape: true,
                outline: 1,
                ..Self::default()
            },
            6 => Self {
                circle: 6,
                opacity: 90,
                ..empty
            },
            7 => Self {
                length: 0,
                dot: 2,
                circle: 8,
                outline: 1,
                ..Self::default()
            },
            _ => Self::default(),
        }
    }

    pub fn value(self, part: CrosshairPart) -> u8 {
        let mut style = self;
        *style.value_mut(part)
    }

    pub fn adjust(&mut self, part: CrosshairPart, delta: i8) {
        let (min, max) = part.bounds();
        let value = self.value_mut(part);
        *value = value.saturating_add_signed(delta).clamp(min, max);
    }

    fn value_mut(&mut self, part: CrosshairPart) -> &mut u8 {
        match part {
            CrosshairPart::Length => &mut self.length,
            CrosshairPart::Thickness => &mut self.thickness,
            CrosshairPart::Gap => &mut self.gap,
            CrosshairPart::Dot => &mut self.dot,
            CrosshairPart::Circle => &mut self.circle,
            CrosshairPart::CircleWidth => &mut self.circle_width,
            CrosshairPart::Outline => &mut self.outline,
            CrosshairPart::Opacity => &mut self.opacity,
        }
    }

    fn normalize(&mut self) {
        for part in CrosshairPart::ALL {
            self.adjust(part, 0);
        }
    }

    pub fn color(self, colorblind: bool) -> Color {
        let [r, g, b] = if colorblind && self.rgb == Self::default().rgb {
            [124, 200, 255]
        } else {
            self.rgb
        };
        Color::srgba_u8(r, g, b, (f32::from(self.opacity) * 2.55).round() as u8)
    }

    pub fn code(self) -> String {
        let [r, g, b] = self.rgb;
        let mut bytes = [
            1,
            self.length,
            self.thickness,
            self.gap,
            self.dot,
            self.circle,
            self.circle_width,
            self.outline,
            u8::from(self.t_shape),
            r,
            g,
            b,
            self.opacity,
            0,
        ];
        bytes[13] = checksum(&bytes[..13]);
        let mut output = String::from("SURFD");
        let mut accumulator = 0u16;
        let mut bits = 0;
        let mut index = 0;
        let mut push = |digit: u8| {
            if index % 5 == 0 {
                output.push('-');
            }
            output.push(char::from(CODE_ALPHABET[digit as usize]));
            index += 1;
        };
        for byte in bytes {
            accumulator = (accumulator << 8) | u16::from(byte);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                push(((accumulator >> bits) & 31) as u8);
            }
            accumulator &= (1 << bits) - 1;
        }
        if bits > 0 {
            push((accumulator << (5 - bits)) as u8);
        }
        output
    }

    pub fn from_code(value: &str) -> Result<Self, &'static str> {
        if value.len() > 64 {
            return Err("Crosshair code is too long.");
        }
        let clean: String = value
            .chars()
            .filter(|character| !character.is_whitespace() && *character != '-')
            .map(|character| character.to_ascii_uppercase())
            .collect();
        let encoded = clean
            .strip_prefix("SURFD")
            .ok_or("Crosshair codes start with SURFD-.")?;
        if encoded.len() != 23 {
            return Err("Enter the complete crosshair code.");
        }
        let mut bytes = [0; 14];
        let mut index = 0;
        let mut accumulator = 0u16;
        let mut bits = 0;
        for character in encoded.bytes() {
            let character = match character {
                b'I' | b'L' => b'1',
                b'O' => b'0',
                character => character,
            };
            let digit = CODE_ALPHABET
                .iter()
                .position(|byte| *byte == character)
                .ok_or("Invalid character in crosshair code.")?;
            accumulator = (accumulator << 5) | digit as u16;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                bytes[index] = (accumulator >> bits) as u8;
                index += 1;
                accumulator &= (1 << bits) - 1;
            }
        }
        if accumulator != 0 || checksum(&bytes[..13]) != bytes[13] {
            return Err("Crosshair code checksum does not match.");
        }
        if bytes[0] != 1 || bytes[8] > 1 {
            return Err("This crosshair code uses an unsupported version.");
        }
        let style = Self {
            length: bytes[1],
            thickness: bytes[2],
            gap: bytes[3],
            dot: bytes[4],
            circle: bytes[5],
            circle_width: bytes[6],
            outline: bytes[7],
            t_shape: bytes[8] == 1,
            rgb: [bytes[9], bytes[10], bytes[11]],
            opacity: bytes[12],
        };
        let mut normalized = style;
        normalized.normalize();
        if normalized != style {
            return Err("Crosshair values are outside the supported range.");
        }
        Ok(style)
    }
}

const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn checksum(bytes: &[u8]) -> u8 {
    let hash = bytes.iter().fold(2_166_136_261u32, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(16_777_619)
    });
    let [a, b, c, d] = hash.to_le_bytes();
    a ^ b ^ c ^ d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosmetic_choices_round_trip_without_resetting_existing_preferences() {
        let equipped = Settings::read(
            br#"{"version":2,"sensitivity":8.0,"avatar_skin":"ember","knife_finish":"glacier"}"#,
        );
        let saved = serde_json::to_vec(&equipped).unwrap();
        let reloaded = Settings::read(&saved);
        assert_eq!(reloaded.avatar_skin, AvatarSkin::Ember);
        assert_eq!(reloaded.knife_finish, KnifeFinish::Glacier);
        assert_eq!(reloaded.sensitivity, 8.0);

        let legacy = Settings::read(br#"{"version":2,"sensitivity":8.0}"#);
        assert_eq!(legacy.avatar_skin, AvatarSkin::Classic);
        assert_eq!(legacy.knife_finish, KnifeFinish::Classic);
        assert_eq!(legacy.sensitivity, 8.0);

        let future = Settings::read(
            br#"{"version":2,"sensitivity":8.0,"avatar_skin":"future","knife_finish":"future"}"#,
        );
        assert_eq!(future.avatar_skin, AvatarSkin::Classic);
        assert_eq!(future.knife_finish, KnifeFinish::Classic);
        assert_eq!(future.sensitivity, 8.0);
    }

    #[test]
    fn original_crosshair_code_matches_and_round_trips() {
        const DEFAULT_CODE: &str = "SURFD-042G2-00000-0G004-4TD46-988";
        let default = CrosshairStyle::default();
        assert_eq!(default.code(), DEFAULT_CODE);
        assert_eq!(CrosshairStyle::from_code(DEFAULT_CODE), Ok(default));
        assert!(CrosshairStyle::from_code("SURFD-042G2-00000-0G004-4TD46-999").is_err());
        for index in 0..8 {
            let style = CrosshairStyle::preset(index);
            assert_eq!(CrosshairStyle::from_code(&style.code()), Ok(style));
        }
    }

    #[test]
    fn source_angles_and_legacy_preferences_preserve_effective_values() {
        let defaults = Settings::default();
        assert!((defaults.vertical_fov_radians().to_degrees() - 73.7398).abs() < 0.0001);
        assert!((defaults.mouse_degrees_per_count() - 0.055).abs() < 1e-12);

        let migrated = Settings::read(br#"{"sensitivity":1.0,"fov":74.0,"show_keys":false}"#);
        assert!((migrated.vertical_fov_radians().to_degrees() - 74.0).abs() < 0.0001);
        assert!((migrated.mouse_degrees_per_count().to_radians() - 0.0022).abs() < 0.00001);
        assert!(!migrated.show_keys);
    }

    #[test]
    fn mouse_and_wheel_bindings_distinguish_edge_and_held_input() {
        let mut settings = Settings::default();
        let keys = ButtonInput::default();
        let mut mouse = ButtonInput::default();
        settings.set_binding(
            ControlAction::Forward,
            Some(0),
            Binding::Mouse(MouseBinding::Back),
        );
        mouse.press(MouseButton::Back);
        assert!(settings.just_pressed(ControlAction::Forward, &keys, &mouse, Vec2::ZERO));
        mouse.clear_just_pressed(MouseButton::Back);
        assert!(!settings.just_pressed(ControlAction::Forward, &keys, &mouse, Vec2::ZERO));
        assert!(settings.active(ControlAction::Forward, &keys, &mouse, Vec2::ZERO));
        assert!(settings.active(ControlAction::Jump, &keys, &mouse, Vec2::NEG_Y));
        assert!(!settings.active(ControlAction::Jump, &keys, &mouse, Vec2::Y));
    }

    #[test]
    fn legacy_key_names_keep_exact_held_and_pressed_matching() {
        use bevy::input::keyboard::NativeKeyCode;

        let mut settings = Settings::read(
            br#"{"version":2,"bindings":{"crouch":[{"key":"ControlRight"}],"restart":[{"key":"Unidentified(Windows(161))"}],"forward":[{"key":"KeyWExtra"}]}}"#,
        );
        let keys = &mut ButtonInput::default();
        let mouse = &ButtonInput::default();

        keys.press(KeyCode::ControlLeft);
        keys.press(KeyCode::KeyW);
        assert!(!settings.active(ControlAction::Crouch, keys, mouse, Vec2::ZERO));
        assert!(!settings.active(ControlAction::Forward, keys, mouse, Vec2::ZERO));
        keys.press(KeyCode::ControlRight);
        assert!(settings.just_pressed(ControlAction::Crouch, keys, mouse, Vec2::ZERO));
        keys.clear_just_pressed(KeyCode::ControlRight);
        assert!(!settings.just_pressed(ControlAction::Crouch, keys, mouse, Vec2::ZERO));
        assert!(settings.active(ControlAction::Crouch, keys, mouse, Vec2::ZERO));

        let custom = KeyCode::Unidentified(NativeKeyCode::Windows(161));
        keys.press(custom);
        assert!(settings.active(ControlAction::Restart, keys, mouse, Vec2::ZERO));
        settings.clear_binding(ControlAction::Restart, 0);
        assert!(!settings.active(ControlAction::Restart, keys, mouse, Vec2::ZERO));
        settings.set_binding(ControlAction::Restart, None, Binding::key(custom));
        let saved = serde_json::to_vec(&settings).unwrap();
        let reloaded = Settings::read(&saved);
        assert!(reloaded.just_pressed(ControlAction::Restart, keys, mouse, Vec2::ZERO));
    }
}
