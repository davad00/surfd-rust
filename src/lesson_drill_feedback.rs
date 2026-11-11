use bevy::math::Vec3;
use bevy_surf::{
    course::{Course, ZoneKind},
    movement::{Player, TICK_SECONDS, native_yaw_to_source, normalize_source_yaw},
    replay::TickCommand,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DrillFeedback {
    pub peak_speed: f32,
    pub finding: Option<DrillFinding>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DrillFinding {
    PadHop {
        milliseconds: u32,
        jumped: bool,
    },
    AirTurn {
        right_degrees: f64,
        paired_degrees: f64,
        strafe_ms: u32,
    },
}

pub(super) struct DrillTrace {
    peak_speed: f32,
    maneuver: Maneuver,
}

enum Maneuver {
    General,
    PadHop {
        bounds: (Vec3, Vec3),
        grounded_ticks: u32,
        jumped: bool,
        finished: bool,
    },
    AirTurn {
        previous_yaw: f64,
        air_ticks: u32,
        strafe_ticks: u32,
        right_degrees: f64,
        paired_degrees: f64,
        finished: bool,
    },
}

impl DrillTrace {
    pub(super) fn new(
        course: &Course,
        section: usize,
        command: TickCommand,
        player: &Player,
    ) -> Self {
        let maneuver = match course.name.as_str() {
            "tutorial-strafe" => Maneuver::AirTurn {
                previous_yaw: yaw(command),
                air_ticks: 0,
                strafe_ticks: 0,
                right_degrees: 0.0,
                paired_degrees: 0.0,
                finished: false,
            },
            "tutorial-bhop" if section > 0 => course
                .zones
                .iter()
                .find_map(|zone| {
                    let ZoneKind::Checkpoint(order) = zone.kind else {
                        return None;
                    };
                    (usize::from(order) == section).then_some(Maneuver::PadHop {
                        bounds: (zone.brush.min, zone.brush.max),
                        grounded_ticks: 0,
                        jumped: false,
                        finished: false,
                    })
                })
                .unwrap_or(Maneuver::General),
            _ => Maneuver::General,
        };
        Self {
            peak_speed: horizontal_speed(player),
            maneuver,
        }
    }

    pub(super) fn observe(&mut self, player: &Player, command: TickCommand) {
        self.peak_speed = self.peak_speed.max(horizontal_speed(player));
        match &mut self.maneuver {
            Maneuver::PadHop {
                bounds: (min, max),
                grounded_ticks,
                jumped,
                finished,
            } if !*finished => {
                let (hull_min, hull_max) = player.hull_bounds();
                let on_pad =
                    player.grounded && hull_min.cmple(*max).all() && hull_max.cmpge(*min).all();
                if on_pad && command.input.jump {
                    *jumped = true;
                    *finished = true;
                } else if on_pad {
                    *grounded_ticks = grounded_ticks.saturating_add(1);
                } else if *grounded_ticks > 0 {
                    *finished = true;
                }
            }
            Maneuver::AirTurn {
                previous_yaw,
                air_ticks,
                strafe_ticks,
                right_degrees,
                paired_degrees,
                finished,
            } if !*finished => {
                let current_yaw = yaw(command);
                let turn = normalize_source_yaw(*previous_yaw - current_yaw).max(0.0);
                *previous_yaw = current_yaw;
                if player.grounded {
                    *finished = *air_ticks > 0;
                } else {
                    *air_ticks = air_ticks.saturating_add(1);
                    *right_degrees += turn;
                    if command.input.strafe > 0.0 {
                        *strafe_ticks = strafe_ticks.saturating_add(1);
                        *paired_degrees += turn;
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn feedback(&self) -> DrillFeedback {
        let finding = match self.maneuver {
            Maneuver::PadHop {
                grounded_ticks,
                jumped,
                ..
            } if grounded_ticks >= 3 => Some(DrillFinding::PadHop {
                milliseconds: milliseconds(grounded_ticks),
                jumped,
            }),
            Maneuver::AirTurn {
                air_ticks,
                strafe_ticks,
                right_degrees,
                paired_degrees,
                ..
            } if air_ticks >= 5 && paired_degrees < 12.0 => Some(DrillFinding::AirTurn {
                right_degrees,
                paired_degrees,
                strafe_ms: milliseconds(strafe_ticks),
            }),
            _ => None,
        };
        DrillFeedback {
            peak_speed: self.peak_speed,
            finding,
        }
    }
}

fn yaw(command: TickCommand) -> f64 {
    command
        .source_yaw_degrees
        .unwrap_or_else(|| native_yaw_to_source(command.input.yaw))
}

fn milliseconds(ticks: u32) -> u32 {
    (f64::from(ticks) * f64::from(TICK_SECONDS) * 1000.0).round() as u32
}

fn horizontal_speed(player: &Player) -> f32 {
    player.velocity.x.hypot(player.velocity.z)
}
