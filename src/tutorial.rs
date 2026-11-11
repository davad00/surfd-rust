//! A progression of short courses built for the normal surf simulation.

use bevy_math::Vec3;

use crate::course::{Course, CourseFog, MapTriggers, Surface, ZoneKind};

#[path = "tutorial_demo.rs"]
mod tutorial_demo;
pub use tutorial_demo::demonstration;

pub struct Lesson {
    pub id: &'static str,
    pub name: &'static str,
    pub goal: &'static str,
    pub instructions: &'static str,
    pub retry_tip: &'static str,
    pub silver_seconds: f64,
    pub gold_seconds: f64,
    pub sections: &'static [LessonSection],
    build: fn(&mut Course),
}

pub struct LessonSection {
    pub name: &'static str,
    pub tip: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Medal {
    Bronze,
    Silver,
    Gold,
}

impl Lesson {
    pub fn medal(&self, seconds: f64) -> Option<Medal> {
        if !seconds.is_finite() || seconds <= 0.0 {
            return None;
        }
        Some(if seconds <= self.gold_seconds {
            Medal::Gold
        } else if seconds <= self.silver_seconds {
            Medal::Silver
        } else {
            Medal::Bronze
        })
    }
}

pub static LESSONS: &[Lesson] = &[
    Lesson {
        id: "tutorial-movement",
        name: "Move & jump",
        goal: "Follow the cyan line around the corner and jump to the gold gate.",
        instructions: "Use the mouse to look and your movement keys to move. Hold {forward} along the line, turn right at the elbow, then press {jump} at the bright takeoff stripe.",
        retry_tip: "Keep moving through the jump. You can press {restart} to try again.",
        silver_seconds: 5.5,
        gold_seconds: 3.6,
        sections: &[
            LessonSection {
                name: "Approach",
                tip: "Follow the cyan line into the turn without slowing down.",
            },
            LessonSection {
                name: "Corner and jump",
                tip: "Turn right at the elbow and keep {forward} held as you jump from the bright stripe.",
            },
        ],
        build: movement,
    },
    Lesson {
        id: "tutorial-catch",
        name: "Catch a ramp",
        goal: "Ride the right face of the ramp to the gold gate.",
        instructions: "Walk off the deck with {forward}, then release it as you fall. On the right face, hold {left} toward the ridge when you drift outward. Ease off near the upper stripe.",
        retry_tip: "Gravity builds speed. Use {left} to stay on the slope and save {forward} for walking on the landing deck.",
        silver_seconds: 5.6,
        gold_seconds: 4.1,
        sections: &[
            LessonSection {
                name: "Ramp entry",
                tip: "Release {forward} as you fall. Tap {left} when you drift away from the ridge.",
            },
            LessonSection {
                name: "Ride and landing",
                tip: "Let the ramp build speed and save {forward} for the landing deck.",
            },
        ],
        build: catch,
    },
    Lesson {
        id: "tutorial-strafe",
        name: "Air-strafe",
        goal: "Curve in the air to reach the offset landing deck.",
        instructions: "Run off the center of the deck. Release {forward}, hold {right} and smoothly turn your mouse to the right. Release {right} and stop turning once you are headed at the gold deck.",
        retry_tip: "Pressing {right} alone only nudges you sideways. Match it with a gentle right turn, around 40 degrees.",
        silver_seconds: 2.2,
        gold_seconds: 1.4,
        sections: &[LessonSection {
            name: "Air turn",
            tip: "Hold {right} as you turn right in the air, then release it when you're aimed at the deck.",
        }],
        build: strafe,
    },
    Lesson {
        id: "tutorial-transfer",
        name: "Switch faces",
        goal: "Ride both ramps and cross the gap between them.",
        instructions: "Use {left} on the right face of the cyan ramp. As you leave its end, use {right} with a small right turn to line up with the left face of the violet ramp. Use {right} toward that ridge.",
        retry_tip: "Look down the second ramp after the transfer. If you miss it low, stay higher on the first face.",
        silver_seconds: 6.8,
        gold_seconds: 5.0,
        sections: &[
            LessonSection {
                name: "First ramp",
                tip: "Hold the cyan ramp high with small taps of {left} toward the ridge.",
            },
            LessonSection {
                name: "Face transfer",
                tip: "Use {right} and a small right turn off the cyan ramp to line up the violet face.",
            },
            LessonSection {
                name: "Second ramp",
                tip: "Look down the violet ramp and use {right} toward its ridge when you drift.",
            },
        ],
        build: transfer,
    },
    Lesson {
        id: "tutorial-rise",
        name: "Gain height",
        goal: "Carry your speed up the rising ramp and reach the high landing.",
        instructions: "Ride high on the right face. Keep {left} toward the ridge through the rising section and let the ramp send you up. Release {left} at the lip and aim for the gold deck.",
        retry_tip: "The kicker redirects your speed upward. Stay near the upper stripe through the bend.",
        silver_seconds: 6.1,
        gold_seconds: 4.5,
        sections: &[
            LessonSection {
                name: "Build speed",
                tip: "Use {left} to stay near the upper stripe while gravity builds speed.",
            },
            LessonSection {
                name: "Kicker and landing",
                tip: "Ride high through the bend and release {left} at the lip.",
            },
        ],
        build: rise,
    },
    Lesson {
        id: "tutorial-line",
        name: "Link a line",
        goal: "Link a left-face ride, a transfer and a high exit in one run.",
        instructions: "Start with {right} on the left face. Line up the violet ramp to your left, then use {left} on its right face. Carry your speed high through the final kicker.",
        retry_tip: "Enter the next face high enough to settle before the kicker. Make small adjustments between ramps.",
        silver_seconds: 8.3,
        gold_seconds: 6.0,
        sections: &[
            LessonSection {
                name: "Left-face ride",
                tip: "Use {right} toward the ridge as you ride the left face.",
            },
            LessonSection {
                name: "Transfer left",
                tip: "Enter the violet ramp high and use {left} toward its ridge.",
            },
            LessonSection {
                name: "High exit",
                tip: "Settle on the second face before the kicker and carry your speed high through it.",
            },
        ],
        build: line,
    },
    Lesson {
        id: "tutorial-bhop",
        name: "Chain bunnyhops",
        goal: "Keep your speed across two violet pads and reach the gold gate.",
        instructions: "Run straight with {forward} and jump with {jump} at the cyan stripe. Jump again as you land on each violet pad, or hold a jump key to auto-hop. Land on the gold deck.",
        retry_tip: "Build speed before the stripe and jump as you touch each violet pad.",
        silver_seconds: 3.7,
        gold_seconds: 3.0,
        sections: &[
            LessonSection {
                name: "First jump",
                tip: "Build speed before the cyan stripe. Jump with {jump} as you reach it.",
            },
            LessonSection {
                name: "Middle hop",
                tip: "Keep {forward} held and jump with {jump} as you touch the violet pad.",
            },
            LessonSection {
                name: "Final hop",
                tip: "Keep your view straight and stop jumping over the gold deck.",
            },
        ],
        build: bhop,
    },
];

pub fn lesson(id: &str) -> Option<&'static Lesson> {
    LESSONS.iter().find(|lesson| lesson.id == id)
}

pub fn course(id: &str) -> Option<Course> {
    let lesson = lesson(id)?;
    let mut course = Course {
        name: lesson.id.to_owned(),
        catalog_revision: None,
        bonus_routes: Vec::new(),
        map_triggers: MapTriggers::default(),
        spawn: Vec3::ZERO,
        spawn_yaw: 0.0,
        brushes: Vec::new(),
        surfaces: Vec::new(),
        textures: Vec::new(),
        visual_meshes: Vec::new(),
        visual_visibility: None,
        practice_spawns: Vec::new(),
        sky: None,
        sky_occluders: Vec::new(),
        fog: Some(CourseFog {
            color: [0.075, 0.135, 0.17],
            start: 1750.0,
            end: 5200.0,
            max_density: 0.85,
        }),
        zones: Vec::new(),
        kill_y: 0.0,
    };
    (lesson.build)(&mut course);
    Some(course)
}

const DECK: [f32; 3] = [0.21, 0.28, 0.33];
const CYAN: [f32; 3] = [0.38, 0.94, 0.85];
const GOLD: [f32; 3] = [1.0, 0.73, 0.34];
const TEAL_RAMP: [f32; 3] = [0.19, 0.52, 0.53];
const VIOLET_RAMP: [f32; 3] = [0.39, 0.35, 0.62];
const VIOLET: [f32; 3] = [0.72, 0.68, 0.97];

fn movement(course: &mut Course) {
    course.spawn = Vec3::new(0.0, 0.05, 30.0);
    course.kill_y = -150.0;
    course.zone([-105., -2., -160.], [105., 132., 110.], ZoneKind::Start);
    deck(course, -100., 100., 0., -275., 110., CYAN);
    deck(course, -100., 370., 0., -490., -275., CYAN);
    deck(course, 480., 820., 0., -490., -275., GOLD);

    for z in (0..330).step_by(38) {
        stripe(course, -5., 5., 0., -(z as f32) - 20., -(z as f32), CYAN);
    }
    for x in (8..370).step_by(38) {
        stripe(course, x as f32, x as f32 + 19., 0., -372., -362., CYAN);
    }
    stripe(course, 320., 350., 0., -478., -286., GOLD);
    course.zone(
        [-100., -1., -348.],
        [100., 108., -334.],
        ZoneKind::Checkpoint(1),
    );

    for [near, far] in [[-490., -478.], [-287., -275.]] {
        course.block([650., 0., near], [670., 146., far], GOLD, false);
    }
    course.block([650., 136., -490.], [670., 146., -275.], GOLD, false);
    stripe(course, 650., 670., 0., -490., -275., GOLD);
    course.zone([650., -2., -488.], [670., 138., -277.], ZoneKind::Finish);
}

fn catch(course: &mut Course) {
    start_deck(course, 72., 520., -245.);
    course.kill_y = -445.;
    ramp(
        course,
        [-38., 440., -245.],
        [-38., 178., -1420.],
        297.,
        TEAL_RAMP,
        CYAN,
    );
    course.zone(
        [-12., -35., -925.],
        [248., 360., -910.],
        ZoneKind::Checkpoint(1),
    );
    finish_deck(course, -130., 340., -286., -1485., -1940.);
}

fn strafe(course: &mut Course) {
    start_deck(course, 0., 520., -230.);
    course.kill_y = -35.;
    finish_deck(course, 90., 390., 170., -353., -492.);
}

fn transfer(course: &mut Course) {
    start_deck(course, 72., 520., -245.);
    course.kill_y = -965.;
    ramp(
        course,
        [-42., 440., -245.],
        [-42., 180., -1240.],
        287.,
        TEAL_RAMP,
        CYAN,
    );
    ramp(
        course,
        [326., -60., -1510.],
        [326., -340., -2480.],
        282.,
        VIOLET_RAMP,
        VIOLET,
    );
    course.zone(
        [-15., -80., -835.],
        [236., 380., -820.],
        ZoneKind::Checkpoint(1),
    );
    course.zone(
        [60., -590., -2020.],
        [328., -40., -2005.],
        ZoneKind::Checkpoint(2),
    );
    finish_deck(course, -15., 425., -765., -2530., -3040.);
}

fn rise(course: &mut Course) {
    start_deck(course, 72., 520., -245.);
    course.kill_y = -105.;
    ramp(
        course,
        [-38., 440., -245.],
        [-38., 215., -1210.],
        297.,
        TEAL_RAMP,
        CYAN,
    );
    ramp(
        course,
        [-38., 215., -1210.],
        [-38., 299., -1400.],
        297.,
        VIOLET_RAMP,
        VIOLET,
    );
    course.zone(
        [-8., -40., -1100.],
        [240., 315., -1080.],
        ZoneKind::Checkpoint(1),
    );
    finish_deck(course, -105., 245., 150., -1500., -1740.);
}

fn line(course: &mut Course) {
    start_deck(course, -72., 650., -245.);
    course.kill_y = -665.;
    ramp(
        course,
        [42., 558., -245.],
        [42., 252., -1460.],
        285.,
        TEAL_RAMP,
        CYAN,
    );
    ramp(
        course,
        [-255., -120., -1785.],
        [-255., -352., -2670.],
        277.,
        VIOLET_RAMP,
        VIOLET,
    );
    ramp(
        course,
        [-255., -352., -2670.],
        [-255., -269., -2860.],
        277.,
        VIOLET_RAMP,
        GOLD,
    );
    course.zone(
        [-235., 5., -975.],
        [10., 450., -960.],
        ZoneKind::Checkpoint(1),
    );
    course.zone(
        [-265., -510., -2380.],
        [-62., -100., -2360.],
        ZoneKind::Checkpoint(2),
    );
    finish_deck(course, -400., -106., -403., -2960., -3510.);
}

fn bhop(course: &mut Course) {
    course.spawn = Vec3::new(0., 0.05, 25.);
    course.kill_y = -155.;
    deck(course, -110., 110., 0., -250., 110., CYAN);
    course.zone([-110., -2., -179.], [110., 108., 110.], ZoneKind::Start);
    for z in [20., -30., -80., -130.] {
        stripe(course, -5., 5., 0., z - 25., z, CYAN);
    }
    stripe(course, -102., 102., 0., -250., -194., CYAN);

    for (number, [far, near]) in [(1, [-462., -370.]), (2, [-660., -550.])] {
        deck(course, -105., 105., 0., far, near, VIOLET);
        stripe(course, -92., 92., 0., near - 14., near, VIOLET);
        course.zone(
            [-105., -2., far],
            [105., 10., near],
            ZoneKind::Checkpoint(number),
        );
    }
    finish_deck(course, -135., 135., 0., -748., -910.);
}

fn start_deck(course: &mut Course, x: f32, y: f32, edge: f32) {
    course.spawn = Vec3::new(x, y + 0.05, 25.);
    deck(course, -182., 182., y, edge, 122., CYAN);
    course.zone(
        [-183., y - 2., edge + 40.],
        [183., y + 112., 124.],
        ZoneKind::Start,
    );
    stripe(course, -182., 182., y, edge, edge + 12., CYAN);
    for z in (0..220).step_by(47) {
        let z = -(z as f32);
        stripe(course, x - 4., x + 4., y, z - 24., z, CYAN);
    }
}

fn finish_deck(course: &mut Course, left: f32, right: f32, y: f32, edge: f32, gate: f32) {
    deck(course, left, right, y, gate - 250., edge, GOLD);
    stripe(course, left, right, y, edge - 12., edge, GOLD);
    stripe(course, left, right, y, gate - 14., gate + 14., GOLD);
    for x in [left, right - 15.] {
        course.block(
            [x, y, gate - 12.],
            [x + 15., y + 138., gate + 12.],
            GOLD,
            false,
        );
    }
    course.block(
        [left, y + 127., gate - 12.],
        [right, y + 138., gate + 12.],
        GOLD,
        false,
    );
    course.zone(
        [left, y - 2., gate - 12.],
        [right, y + 123., gate + 12.],
        ZoneKind::Finish,
    );
}

fn deck(course: &mut Course, left: f32, right: f32, y: f32, far: f32, near: f32, accent: [f32; 3]) {
    course.block([left, y - 36., far], [right, y, near], DECK, true);
    for x in [left + 6., right - 12.] {
        stripe(course, x, x + 6., y, far, near, accent);
    }
}

fn stripe(
    course: &mut Course,
    left: f32,
    right: f32,
    y: f32,
    far: f32,
    near: f32,
    color: [f32; 3],
) {
    let y = y + 0.15;
    course.surfaces.push(Surface {
        vertices: vec![
            Vec3::new(left, y, near),
            Vec3::new(right, y, near),
            Vec3::new(right, y, far),
            Vec3::new(left, y, far),
        ],
        color,
        texture: None,
        lightmap: None,
    });
}

fn ramp(
    course: &mut Course,
    near: [f32; 3],
    far: [f32; 3],
    width: f32,
    color: [f32; 3],
    accent: [f32; 3],
) {
    let near = Vec3::from_array(near);
    let far = Vec3::from_array(far);
    let left = Vec3::new(-width, -width * 1.48, 0.);
    let right = Vec3::new(width, left.y, 0.);
    course.polyhedron(
        &[
            near + left,
            near,
            near + right,
            far + left,
            far,
            far + right,
        ],
        &[
            &[0, 1, 2],
            &[3, 5, 4],
            &[0, 3, 4, 1],
            &[1, 4, 5, 2],
            &[0, 2, 5, 3],
        ],
        color,
        true,
    );
    for side in [left, right] {
        let point = |along: f32, across: f32| near.lerp(far, along) + side * across + Vec3::Y * 0.7;
        let mut mark = |from: f32, to: f32, inner: f32, outer: f32, color| {
            let mut vertices = vec![
                point(from, inner),
                point(from, outer),
                point(to, outer),
                point(to, inner),
            ];
            if side.x < 0.0 {
                vertices.reverse();
            }
            course.surfaces.push(Surface {
                vertices,
                color,
                texture: None,
                lightmap: None,
            });
        };
        for (inner, outer) in [(0.06, 0.084), (0.42, 0.434), (0.94, 0.958)] {
            mark(0., 1., inner, outer, accent);
        }
        let ticks = ((near.z - far.z) / 110.).round() as u32;
        for tick in 1..ticks {
            let t = tick as f32 / ticks as f32;
            mark(t, t + 0.004, 0.08, 0.94, color.map(|value| value * 0.77));
        }
    }
}
