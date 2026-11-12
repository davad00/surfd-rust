use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Loadout {
    pub avatar_skin: AvatarSkin,
    pub knife_finish: KnifeFinish,
}

impl Loadout {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", from = "String")]
pub enum AvatarSkin {
    #[default]
    Classic,
    Glacier,
    Ember,
}

impl AvatarSkin {
    pub const ALL: [Self; 3] = [Self::Classic, Self::Glacier, Self::Ember];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Classic => "Classic",
            Self::Glacier => "Glacier",
            Self::Ember => "Ember",
        }
    }

    pub const fn tint(self) -> Option<[u8; 3]> {
        match self {
            Self::Classic => None,
            Self::Glacier => Some([101, 201, 243]),
            Self::Ember => Some([255, 124, 76]),
        }
    }

    pub const fn swatch(self) -> [u8; 3] {
        match self.tint() {
            Some(tint) => tint,
            None => [205, 204, 196],
        }
    }
}

impl From<String> for AvatarSkin {
    fn from(id: String) -> Self {
        match id.as_str() {
            "glacier" => Self::Glacier,
            "ember" => Self::Ember,
            _ => Self::Classic,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", from = "String")]
pub enum KnifeFinish {
    #[default]
    Classic,
    Glacier,
    Ember,
}

pub struct KnifePalette {
    pub blade: [u8; 3],
    pub bevel: [u8; 3],
    pub handle: [u8; 3],
    pub grip: [u8; 3],
}

impl KnifeFinish {
    pub const ALL: [Self; 3] = [Self::Classic, Self::Glacier, Self::Ember];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Classic => "Classic Steel",
            Self::Glacier => "Glacier",
            Self::Ember => "Ember",
        }
    }

    pub const fn palette(self) -> KnifePalette {
        match self {
            Self::Classic => KnifePalette {
                blade: [146, 162, 173],
                bevel: [224, 236, 243],
                handle: [39, 43, 50],
                grip: [100, 115, 124],
            },
            Self::Glacier => KnifePalette {
                blade: [78, 170, 203],
                bevel: [199, 244, 255],
                handle: [26, 44, 56],
                grip: [74, 155, 185],
            },
            Self::Ember => KnifePalette {
                blade: [201, 72, 47],
                bevel: [255, 183, 117],
                handle: [55, 31, 29],
                grip: [207, 85, 58],
            },
        }
    }
}

impl From<String> for KnifeFinish {
    fn from(id: String) -> Self {
        match id.as_str() {
            "glacier" => Self::Glacier,
            "ember" => Self::Ember,
            _ => Self::Classic,
        }
    }
}
