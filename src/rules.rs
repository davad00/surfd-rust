use crate::course::Course;

/// The shared 15 ms movement preset with the course's speed and incline settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MovementRules {
    pub max_velocity: f32,
    pub incline_fix: bool,
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
        }
    }
}

impl MovementRules {
    pub fn for_course(course: &Course) -> Self {
        if let Some(timer) = &course.map_triggers.source_timer {
            return Self {
                max_velocity: timer.max_velocity.unwrap_or(Self::default().max_velocity),
                ..Self::default()
            };
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
        }
    }
}
