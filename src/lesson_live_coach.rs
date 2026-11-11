use bevy_math::Vec3;
use bevy_surf::{
    course::{Brush, Course, ZoneKind},
    movement::{Player, native_yaw_to_source, normalize_source_yaw},
    replay::TickCommand,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveLessonCue {
    ReleaseForward,
    SteerLeft,
    SwitchRight,
    HopOnLanding,
    StayHigh,
}

#[derive(Clone, Copy)]
pub(super) struct LiveLessonCoach {
    maneuver: Maneuver,
    forward_yaw: f64,
    held_ticks: u8,
    pub(super) cue: Option<LiveLessonCue>,
}

#[derive(Clone, Copy)]
enum Maneuver {
    RampEntry {
        deck_edge: f32,
        outside_line: f32,
    },
    FaceTransfer {
        cyan_end: f32,
        violet_front: f32,
    },
    Bunnyhop {
        pads: [Pad; 2],
    },
    Kicker {
        approach: f32,
        lip: f32,
        ridge_x: f32,
    },
}

#[derive(Clone, Copy)]
struct Pad {
    min: Vec3,
    max: Vec3,
}

impl LiveLessonCoach {
    pub(super) fn for_course(course: &Course) -> Option<Self> {
        let maneuver = match course.name.as_str() {
            "tutorial-catch" => {
                let spawn = course.spawn;
                let deck = course.brushes.iter().find(|brush| {
                    (brush.min.x..=brush.max.x).contains(&spawn.x)
                        && (brush.min.z..=brush.max.z).contains(&spawn.z)
                        && (spawn.y - brush.max.y).abs() < 1.0
                })?;
                let ramp = course.brushes.iter().find(|brush| is_surf_ramp(brush))?;
                Maneuver::RampEntry {
                    deck_edge: deck.min.z,
                    outside_line: ramp.max.x - (ramp.max.x - ramp.min.x) * 0.2,
                }
            }
            "tutorial-transfer" => {
                let mut ramps = course.brushes.iter().filter(|brush| is_surf_ramp(brush));
                let cyan_end = ramps.next()?.min.z;
                let violet_front = ramps.next()?.max.z;
                if cyan_end <= violet_front {
                    return None;
                }
                Maneuver::FaceTransfer {
                    cyan_end,
                    violet_front,
                }
            }
            "tutorial-bhop" => Maneuver::Bunnyhop {
                pads: [
                    Pad::for_checkpoint(course, 1)?,
                    Pad::for_checkpoint(course, 2)?,
                ],
            },
            "tutorial-rise" => {
                let kicker = course.brushes.iter().rfind(|brush| is_surf_ramp(brush))?;
                let length = kicker.max.z - kicker.min.z;
                Maneuver::Kicker {
                    approach: kicker.max.z + 2.0 * length,
                    lip: kicker.min.z,
                    ridge_x: (kicker.min.x + kicker.max.x) * 0.5,
                }
            }
            _ => return None,
        };
        Some(Self {
            maneuver,
            forward_yaw: native_yaw_to_source(course.spawn_yaw),
            held_ticks: 0,
            cue: None,
        })
    }

    pub(super) fn supports_section(&self, section: usize) -> bool {
        match self.maneuver {
            Maneuver::RampEntry { .. } => section == 0,
            Maneuver::FaceTransfer { .. } => section == 1,
            Maneuver::Bunnyhop { .. } => section <= 2,
            Maneuver::Kicker { .. } => section <= 1,
        }
    }

    pub(super) fn reset(&mut self) {
        self.held_ticks = 0;
        self.cue = None;
    }

    pub(super) fn observe(&mut self, player: &Player, command: TickCommand) {
        if let Maneuver::Bunnyhop { pads } = self.maneuver {
            self.cue = (!command.input.jump && pads.iter().any(|pad| pad.contact_soon(player)))
                .then_some(LiveLessonCue::HopOnLanding);
            return;
        }
        if player.grounded {
            self.reset();
            return;
        }
        let yaw = command
            .source_yaw_degrees
            .unwrap_or_else(|| native_yaw_to_source(command.input.yaw));
        let looking_down_ramp = normalize_source_yaw(yaw - self.forward_yaw).abs() <= 30.0;
        let (min, max) = player.hull_bounds();
        self.cue = match self.maneuver {
            Maneuver::RampEntry {
                deck_edge,
                outside_line,
            } if player.velocity.y < 0.0 && max.z <= deck_edge => {
                self.count_held(command.input.forward > 0.0);
                if looking_down_ramp
                    && player.position.x >= outside_line
                    && player.velocity.x > 30.0
                    && command.input.strafe >= 0.0
                {
                    Some(LiveLessonCue::SteerLeft)
                } else {
                    (self.held_ticks >= 5).then_some(LiveLessonCue::ReleaseForward)
                }
            }
            Maneuver::FaceTransfer {
                cyan_end,
                violet_front,
            } if looking_down_ramp
                && player.velocity.z < 0.0
                && max.z <= cyan_end
                && min.z >= violet_front =>
            {
                self.count_held(command.input.strafe < 0.0);
                (self.held_ticks >= 3).then_some(LiveLessonCue::SwitchRight)
            }
            Maneuver::Kicker {
                approach,
                lip,
                ridge_x,
            } if looking_down_ramp
                && player.position.z > lip
                && player.position.z <= approach
                && player.position.x > ridge_x
                && player.velocity.z < 0.0
                && player.velocity.x > 22.0
                && command.input.strafe >= 0.0 =>
            {
                Some(LiveLessonCue::StayHigh)
            }
            _ => {
                self.held_ticks = 0;
                None
            }
        };
    }

    fn count_held(&mut self, held: bool) {
        self.held_ticks = if held {
            self.held_ticks.saturating_add(1)
        } else {
            0
        };
    }
}

impl Pad {
    fn for_checkpoint(course: &Course, order: u16) -> Option<Self> {
        let zone = &course
            .zones
            .iter()
            .find(|zone| matches!(zone.kind, ZoneKind::Checkpoint(index) if index == order))?
            .brush;
        let deck = course.brushes.iter().find(|brush| {
            brush.min.x == zone.min.x
                && brush.max.x == zone.max.x
                && brush.min.z == zone.min.z
                && brush.max.z == zone.max.z
                && (zone.min.y..=zone.max.y).contains(&brush.max.y)
        })?;
        Some(Self {
            min: deck.min,
            max: deck.max,
        })
    }

    fn contact_soon(&self, player: &Player) -> bool {
        let height = player.position.y - self.max.y;
        let time = if player.grounded && height.abs() < 1.0 {
            0.0
        } else if !player.grounded && height >= 0.0 && player.velocity.y < 0.0 {
            // Current vertical speed gives an upper bound on arrival during descent.
            height / -player.velocity.y
        } else {
            return false;
        };
        if time > 0.35 {
            return false;
        }
        let (min, max) = player.hull_bounds();
        let offset = player.velocity * time;
        min.x + offset.x <= self.max.x
            && max.x + offset.x >= self.min.x
            && min.z + offset.z <= self.max.z
            && max.z + offset.z >= self.min.z
    }
}

fn is_surf_ramp(brush: &Brush) -> bool {
    brush
        .planes
        .iter()
        .any(|plane| plane.normal.x > 0.25 && plane.normal.y > 0.0 && plane.normal.y < 0.7)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_surf::{
        session::{RunPhase, Session},
        tutorial,
    };

    #[test]
    fn authored_demos_complete_without_correction_prompts() {
        for id in [
            "tutorial-catch",
            "tutorial-transfer",
            "tutorial-rise",
            "tutorial-bhop",
        ] {
            let course = tutorial::course(id).unwrap();
            let mut coach = LiveLessonCoach::for_course(&course).unwrap();
            let demo = tutorial::demonstration(&course).unwrap();
            let mut run = Session::new(&course);
            let mut sampled = 0;
            for command in demo.commands {
                if run.phase == RunPhase::Running && coach.supports_section(run.splits.len()) {
                    coach.observe(&run.player, command);
                    assert_eq!(
                        coach.cue, None,
                        "clean {id} command at {:?}",
                        run.player.position
                    );
                    sampled += 1;
                }
                run.tick_command(&course, command);
            }
            assert!(sampled > 0);
            assert_eq!(run.phase, RunPhase::Finished);
        }
    }
}
