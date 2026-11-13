use crate::course::Course;
use serde::Deserialize;

const DEFAULT_GRAVITY: f32 = 800.0;
const DEFAULT_AIR_ACCELERATION: f32 = 150.0;
const DEFAULT_GROUND_ACCELERATION: f32 = 5.0;
const DEFAULT_GROUND_FRICTION: f32 = 4.0;
const DEFAULT_STOP_SPEED: f32 = 75.0;

/// The shared 15 ms movement preset with the course's scalar overrides.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovementRules {
    pub max_velocity: f32,
    pub incline_fix: bool,
    pub gravity: f32,
    pub air_acceleration: f32,
    pub ground_acceleration: f32,
    pub ground_friction: f32,
    pub stop_speed: f32,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceMovementSettings {
    pub gravity: Option<f32>,
    pub air_acceleration: Option<f32>,
    pub ground_acceleration: Option<f32>,
    pub ground_friction: Option<f32>,
    pub stop_speed: Option<f32>,
}

impl SourceMovementSettings {
    pub(crate) fn validate(self) -> Result<(), String> {
        for (name, value, max) in [
            ("gravity", self.gravity, 32_768.0),
            ("air_acceleration", self.air_acceleration, 4096.0),
            ("ground_acceleration", self.ground_acceleration, 4096.0),
            ("ground_friction", self.ground_friction, 128.0),
            ("stop_speed", self.stop_speed, 32_768.0),
        ] {
            if value.is_some_and(|value| !(0.0..=max).contains(&value)) {
                return Err(format!(
                    "Imported Source movement setting {name} must be between 0 and {max}"
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn changes_defaults(self) -> bool {
        let defaults = MovementRules::default();
        self.apply(defaults) != defaults
    }

    fn apply(self, rules: MovementRules) -> MovementRules {
        MovementRules {
            gravity: self.gravity.unwrap_or(rules.gravity),
            air_acceleration: self.air_acceleration.unwrap_or(rules.air_acceleration),
            ground_acceleration: self
                .ground_acceleration
                .unwrap_or(rules.ground_acceleration),
            ground_friction: self.ground_friction.unwrap_or(rules.ground_friction),
            stop_speed: self.stop_speed.unwrap_or(rules.stop_speed),
            ..rules
        }
    }
}

/// The rules attached to a native board or replay, independent of its command format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PhysicsProfile {
    Legacy,
    AuthoredContacts,
    Surfd,
    SurfdBonus,
}

impl PhysicsProfile {
    const CONTACTS_SUFFIX: &str = ":contacts-v1";
    const SURFD_SUFFIX: &str = ":surfd-v2";
    const SURFD_BONUS_SUFFIX: &str = ":surfd-bonus-v1";

    pub(crate) fn uses_surfd(self) -> bool {
        matches!(self, Self::Surfd | Self::SurfdBonus)
    }

    pub(crate) fn revision(self, mut base: String) -> String {
        base.push_str(match self {
            Self::Legacy => "",
            Self::AuthoredContacts => Self::CONTACTS_SUFFIX,
            Self::Surfd => Self::SURFD_SUFFIX,
            Self::SurfdBonus => Self::SURFD_BONUS_SUFFIX,
        });
        base
    }

    pub(crate) fn split_revision(revision: &str) -> (&str, Self) {
        if let Some(base) = revision.strip_suffix(Self::SURFD_BONUS_SUFFIX) {
            (base, Self::SurfdBonus)
        } else if let Some(base) = revision.strip_suffix(Self::SURFD_SUFFIX) {
            (base, Self::Surfd)
        } else if let Some(base) = revision.strip_suffix(Self::CONTACTS_SUFFIX) {
            (base, Self::AuthoredContacts)
        } else {
            (revision, Self::Legacy)
        }
    }
}

impl Default for MovementRules {
    fn default() -> Self {
        Self {
            max_velocity: 5000.0,
            incline_fix: true,
            gravity: DEFAULT_GRAVITY,
            air_acceleration: DEFAULT_AIR_ACCELERATION,
            ground_acceleration: DEFAULT_GROUND_ACCELERATION,
            ground_friction: DEFAULT_GROUND_FRICTION,
            stop_speed: DEFAULT_STOP_SPEED,
        }
    }
}

impl MovementRules {
    pub fn for_course(course: &Course) -> Self {
        if let Some(timer) = &course.map_triggers.source_timer {
            return timer.movement_settings.apply(Self {
                max_velocity: timer.max_velocity.unwrap_or(Self::default().max_velocity),
                ..Self::default()
            });
        }
        let max_velocity = match course.name.as_str() {
            "surf_aircontrol_ksf"
            | "surf_summit"
            | "surf_pantheon"
            | "surf_nuclear"
            | "surf_listless_ksf"
            | "surf_666"
            | "surf_lt_omnific"
            | "surf_stonework4"
            | "surf_radiant"
            | "surf_race_final"
            | "surf_me"
            | "surf_sofia_fix"
            | "surf_interference"
            | "surf_leidenfrost"
            | "surf_not_quite_goliath" => 10000.0,
            "surf_cyberwave" | "surf_sandtrap2" => 7500.0,
            "surf_nyx" | "surf_reprise" | "surf_fiellu_ksf" => 4000.0,
            "surf_fruits"
            | "surf_utopia_njv"
            | "surf_mesa_fixed"
            | "surf_kitsune"
            | "surf_lux"
            | "surf_beginner"
            | "surf_year3000"
            | "surf_ace"
            | "surf_legends"
            | "surf_eclipse"
            | "surf_summer_ksf"
            | "surf_forbidden_ways_ksf"
            | "surf_derpis_ksf"
            | "surf_prelude_fix"
            | "surf_rookie"
            | "surf_whiteout"
            | "surf_frost"
            | "surf_aura"
            | "surf_deathstar"
            | "surf_artois"
            | "surf_orthodox"
            | "surf_rebel_resistance_final3"
            | "surf_minuet_ksf"
            | "surf_hades2"
            | "surf_angst"
            | "surf_technique"
            | "surf_bugs"
            | "surf_sinsane_ksf"
            | "surf_santorini_ksf"
            | "surf_botanica"
            | "surf_surreal"
            | "surf_anzchamps"
            | "surf_premium"
            | "surf_ravine"
            | "surf_agony"
            | "surf_kitsune2"
            | "surf_overgrowth" => 3500.0,
            _ => 5000.0,
        };
        Self {
            max_velocity,
            incline_fix: !matches!(
                course.name.as_str(),
                "surf_fruits" | "surf_fiellu_ksf" | "surf_phantasia"
            ),
            ..Self::default()
        }
    }

    pub(crate) fn half_gravity_step(self) -> f32 {
        self.gravity * crate::movement::TICK_SECONDS * 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_movement_settings_keep_defaults_and_reject_invalid_values() {
        let defaults = MovementRules::default();
        let parse = |input: &str| serde_json::from_str::<SourceMovementSettings>(input);
        let omitted = parse("{}").unwrap();
        let explicit_default = parse(r#"{"gravity":800,"air_acceleration":150,"ground_acceleration":5,"ground_friction":4,"stop_speed":75}"#).unwrap();
        assert!(!omitted.changes_defaults());
        assert!(!explicit_default.changes_defaults());
        assert_eq!(explicit_default.apply(defaults), defaults);
        let overrides =
            parse(r#"{"gravity":400,"air_acceleration":30,"ground_friction":8}"#).unwrap();
        overrides.validate().unwrap();
        assert!(overrides.changes_defaults());
        let result = overrides.apply(defaults);
        assert_eq!(
            (
                result.gravity,
                result.air_acceleration,
                result.ground_friction
            ),
            (400.0, 30.0, 8.0)
        );
        assert_eq!(
            (
                result.max_velocity,
                result.ground_acceleration,
                result.stop_speed
            ),
            (5000.0, 5.0, 75.0)
        );
        assert!(parse(r#"{"gravity":-1}"#).unwrap().validate().is_err());
        assert!(
            parse(r#"{"ground_friction":129}"#)
                .unwrap()
                .validate()
                .is_err()
        );
        assert!(parse(r#"{"tickrate":100}"#).is_err());
    }
}
