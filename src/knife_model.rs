//! Original full-tang field knife geometry and vertex-painted finishes.

use bevy::{color::ColorToComponents, prelude::*};

use crate::cosmetics::{KnifeFinish, KnifePalette};

use super::Model;

type Rgb = [u8; 3];

pub(super) fn build(finish: KnifeFinish) -> Model {
    let palette = finish.palette();
    let mut model = Model::default();
    blade(&mut model, finish, &palette);
    grip(&mut model, &palette);
    guard(&mut model, &palette);
    model
}

struct BladeRow {
    y: f32,
    edge: f32,
    grind: f32,
    swedge: f32,
    spine: f32,
    thickness: f32,
    lip: f32,
    reflection: f32,
    relief: f32,
}

impl BladeRow {
    const fn new(
        y: f32,
        [edge, grind, swedge, spine]: [f32; 4],
        thickness: f32,
        lip: f32,
        reflection: f32,
        relief: f32,
    ) -> Self {
        Self {
            y,
            edge,
            grind,
            swedge,
            spine,
            thickness,
            lip,
            reflection,
            relief,
        }
    }

    fn rails(&self) -> [Vec3; 8] {
        let center = self.grind + (self.swedge - self.grind) * 0.61;
        let relief = (self.swedge - self.grind) * self.relief * 0.15;
        [
            Vec3::new(self.edge, self.y, self.lip),
            Vec3::new(
                self.edge + (self.grind - self.edge) * 0.15,
                self.y,
                self.lip + (self.thickness - self.lip) * 0.20,
            ),
            Vec3::new(self.grind, self.y, self.thickness),
            Vec3::new(center - relief, self.y, self.thickness),
            Vec3::new(center, self.y, self.thickness * (1.0 - 0.25 * self.relief)),
            Vec3::new(center + relief, self.y, self.thickness),
            Vec3::new(self.swedge, self.y, self.thickness),
            Vec3::new(self.spine, self.y, self.thickness * 0.68),
        ]
    }
}

// The outer rails form a swept edge and a drop-point spine. Thickness and
// bevels taper into the same point on both faces.
const BLADE: [BladeRow; 10] = [
    BladeRow::new(
        0.016,
        [-0.031, -0.016, 0.019, 0.025],
        0.010,
        0.007,
        0.20,
        0.0,
    ),
    BladeRow::new(
        0.044,
        [-0.034, -0.016, 0.020, 0.027],
        0.011,
        0.005,
        0.26,
        0.0,
    ),
    BladeRow::new(
        0.062,
        [-0.041, -0.016, 0.020, 0.028],
        0.011,
        0.001,
        0.13,
        0.45,
    ),
    BladeRow::new(
        0.112,
        [-0.043, -0.013, 0.021, 0.030],
        0.011,
        0.0007,
        0.42,
        1.0,
    ),
    BladeRow::new(
        0.175,
        [-0.040, -0.012, 0.019, 0.028],
        0.010,
        0.0006,
        0.38,
        1.0,
    ),
    BladeRow::new(
        0.226,
        [-0.034, -0.010, 0.016, 0.025],
        0.009,
        0.0005,
        0.21,
        0.74,
    ),
    BladeRow::new(
        0.273,
        [-0.024, -0.006, 0.010, 0.019],
        0.007,
        0.0004,
        0.45,
        0.0,
    ),
    BladeRow::new(
        0.293,
        [-0.017, -0.003, 0.007, 0.014],
        0.005,
        0.0003,
        0.51,
        0.0,
    ),
    BladeRow::new(
        0.306,
        [-0.012, 0.000, 0.005, 0.011],
        0.0035,
        0.0002,
        0.40,
        0.0,
    ),
    BladeRow::new(0.328, [0.003, 0.003, 0.003, 0.003], 0.0, 0.0, 0.56, 0.0),
];

fn blade(model: &mut Model, finish: KnifeFinish, palette: &KnifePalette) {
    for rows in BLADE.windows(2) {
        let low = rows[0].rails();
        let high = rows[1].rails();
        for band in 0..low.len() - 1 {
            let [low_left, low_right] = steel_tones(finish, palette, &rows[0], band);
            let [high_left, high_right] = steel_tones(finish, palette, &rows[1], band);
            double_face(
                model,
                [low[band], low[band + 1], high[band + 1], high[band]],
                [low_left, low_right, high_right, high_left],
            );
        }
        let edge = palette.bevel;
        let spine = blend(palette.blade, [32, 41, 48], 0.40);
        let mirror = |p: Vec3| Vec3::new(p.x, p.y, -p.z);
        quad(
            model,
            [mirror(low[0]), low[0], high[0], mirror(high[0])],
            [edge; 4],
        );
        quad(
            model,
            [low[7], mirror(low[7]), mirror(high[7]), high[7]],
            [spine; 4],
        );
    }
    decorate_blade(model, finish, palette);
}

fn steel_tones(
    finish: KnifeFinish,
    palette: &KnifePalette,
    row: &BladeRow,
    band: usize,
) -> [Rgb; 2] {
    let length = ((row.y - 0.02) / 0.31).clamp(0.0, 1.0);
    let base = match finish {
        KnifeFinish::Classic => blend(palette.blade, [201, 212, 219], row.reflection * 0.55),
        KnifeFinish::Glacier => {
            let indigo = blend([27, 42, 90], [75, 86, 169], (length * 2.6).min(1.0));
            blend(
                indigo,
                [53, 200, 209],
                ((length - 0.29) * 1.55).clamp(0.0, 1.0),
            )
        }
        KnifeFinish::Ember => {
            let copper = blend([45, 32, 43], [172, 65, 50], (length * 2.0).min(1.0));
            blend(
                copper,
                [237, 172, 77],
                ((length - 0.42) * 1.73).clamp(0.0, 1.0),
            )
        }
    };
    let face = blend(base, palette.bevel, row.reflection * 0.42);
    let edge = blend(palette.bevel, [255, 255, 252], 0.40);
    let grind = blend(base, palette.bevel, 0.43 + row.reflection * 0.27);
    let ridge = blend(face, palette.bevel, 0.13);
    let groove = blend(face, [24, 37, 45], row.relief * 0.66);
    let swedge = blend(base, palette.bevel, 0.57 + row.reflection * 0.18);
    match band {
        0 => [edge, palette.bevel],
        1 => [grind, face],
        2 => [ridge, face],
        3 => [face, groove],
        4 => [groove, face],
        5 => [face, ridge],
        _ => [swedge, blend(swedge, base, 0.24)],
    }
}

fn decorate_blade(model: &mut Model, finish: KnifeFinish, palette: &KnifePalette) {
    const GRIND: [usize; 2] = [1, 2];
    const FLAT: [usize; 2] = [2, 3];
    const SPINE_FLAT: [usize; 2] = [5, 6];
    let light = |amount| blend(palette.blade, palette.bevel, amount);
    match finish {
        KnifeFinish::Classic => {
            // Satin brush marks run lengthwise and stop short of the polish.
            for (start, end, across, width, tone) in [
                (0.066, 0.248, 0.30, 0.045, 0.47),
                (0.082, 0.285, 0.52, 0.065, 0.29),
                (0.070, 0.267, 0.75, 0.045, 0.52),
            ] {
                stroke(
                    model,
                    GRIND,
                    &[(start, across), (end, across + 0.025)],
                    width,
                    0.0008,
                    light(tone),
                );
            }
        }
        KnifeFinish::Glacier => {
            // A shadowed ice vein and a second fracture along the spine.
            let vein = [
                (0.055, 0.22),
                (0.085, 0.57),
                (0.110, 0.36),
                (0.141, 0.75),
                (0.173, 0.42),
                (0.211, 0.68),
                (0.251, 0.39),
                (0.284, 0.69),
            ];
            stroke(model, FLAT, &vein, 0.37, 0.0008, [29, 106, 134]);
            stroke(model, FLAT, &vein, 0.19, 0.0010, [162, 242, 234]);
            stroke(
                model,
                SPINE_FLAT,
                &[
                    (0.087, 0.63),
                    (0.126, 0.34),
                    (0.155, 0.68),
                    (0.202, 0.27),
                    (0.243, 0.60),
                ],
                0.24,
                0.0009,
                [94, 193, 224],
            );
        }
        KnifeFinish::Ember => {
            // Offset contour lines follow a copper-to-straw heat gradient.
            stroke(
                model,
                FLAT,
                &[
                    (0.066, 0.34),
                    (0.099, 0.44),
                    (0.134, 0.42),
                    (0.177, 0.60),
                    (0.220, 0.54),
                    (0.261, 0.74),
                    (0.289, 0.73),
                ],
                0.26,
                0.0008,
                [84, 35, 47],
            );
            stroke(
                model,
                FLAT,
                &[
                    (0.072, 0.54),
                    (0.106, 0.64),
                    (0.142, 0.58),
                    (0.182, 0.77),
                    (0.219, 0.70),
                    (0.261, 0.84),
                    (0.281, 0.82),
                ],
                0.17,
                0.0010,
                [255, 197, 115],
            );
            stroke(
                model,
                SPINE_FLAT,
                &[
                    (0.101, 0.16),
                    (0.153, 0.23),
                    (0.184, 0.43),
                    (0.213, 0.40),
                    (0.257, 0.53),
                ],
                0.22,
                0.0009,
                [107, 40, 44],
            );
        }
    }
}

fn stroke(
    model: &mut Model,
    [left, right]: [usize; 2],
    line: &[(f32, f32)],
    width: f32,
    height: f32,
    color: Rgb,
) {
    // All finishes reserve the same number of vertices per stroke. The
    // merged room avatar can change a finish without changing its budget.
    const SEGMENTS: usize = 10;
    let start = line[0].0;
    let end = line[line.len() - 1].0;
    let points = |i: usize| {
        let along = i as f32 / SEGMENTS as f32;
        let y = start + (end - start) * along;
        let anchors = line
            .windows(2)
            .find(|rows| y <= rows[1].0)
            .unwrap_or(&line[line.len() - 2..]);
        let [(lower, from), (upper, to)] = [anchors[0], anchors[1]];
        let middle = from + (to - from) * ((y - lower) / (upper - lower));
        let rows = BLADE.windows(2).find(|rows| y <= rows[1].y).unwrap();
        let low = rows[0].rails();
        let high = rows[1].rails();
        let taper = (along * 5.0).min((1.0 - along) * 5.0).clamp(0.28, 1.0);
        let point = |side| {
            let across = middle + side * width * taper * 0.5;
            low[left].lerp(low[right], across).lerp(
                high[left].lerp(high[right], across),
                (y - rows[0].y) / (rows[1].y - rows[0].y),
            ) + Vec3::Z * height
        };
        [point(-1.0), point(1.0)]
    };
    let mut previous = points(0);
    for i in 1..=SEGMENTS {
        let next = points(i);
        double_face(
            model,
            [previous[0], previous[1], next[1], next[0]],
            [color; 4],
        );
        previous = next;
    }
}

// Y, center X, half width and half thickness of the overmold. The shaped
// waist and chamfered ends keep the grip readable at a distance.
const GRIP: [[f32; 4]; 8] = [
    [-0.186, 0.003, 0.020, 0.014],
    [-0.176, 0.003, 0.026, 0.020],
    [-0.147, 0.002, 0.028, 0.022],
    [-0.112, 0.003, 0.028, 0.023],
    [-0.073, 0.005, 0.025, 0.022],
    [-0.039, 0.006, 0.022, 0.019],
    [-0.014, 0.003, 0.024, 0.019],
    [0.008, 0.000, 0.027, 0.020],
];
const CROSS_SECTION: [[f32; 2]; 12] = [
    [-1.0, 0.0],
    [-0.94, 0.49],
    [-0.63, 0.89],
    [-0.29, 1.0],
    [0.29, 1.0],
    [0.63, 0.89],
    [0.94, 0.49],
    [1.0, 0.0],
    [0.91, -0.58],
    [0.54, -1.0],
    [-0.54, -1.0],
    [-0.91, -0.58],
];

fn grip(model: &mut Model, palette: &KnifePalette) {
    let graphite = blend(palette.handle, [45, 49, 53], 0.55);
    let tang = blend([46, 54, 60], palette.blade, 0.34);
    let ring = |[y, center, width, depth]: [f32; 4], side| {
        let [x, z] = CROSS_SECTION[side % CROSS_SECTION.len()];
        Vec3::new(center + x * width, y, z * depth)
    };
    for (row, pair) in GRIP.windows(2).enumerate() {
        for side in 0..CROSS_SECTION.len() {
            let next = side + 1;
            let metal = row == 0 || side == 0 || side == 6 || side == 7 || side == 11;
            let tone = if metal { tang } else { graphite };
            let glint = blend(tone, [108, 116, 122], if metal { 0.27 } else { 0.09 });
            quad(
                model,
                [
                    ring(pair[0], side),
                    ring(pair[0], next),
                    ring(pair[1], next),
                    ring(pair[1], side),
                ],
                [tone, tone, glint, glint],
            );
        }
    }
    for (section, top) in [(GRIP[0], false), (GRIP[GRIP.len() - 1], true)] {
        for side in 0..CROSS_SECTION.len() {
            let a = ring(section, side);
            let b = ring(section, side + 1);
            let center = Vec3::new(section[1], section[0], 0.0);
            triangle(
                model,
                if top { [center, a, b] } else { [center, b, a] },
                [tang; 3],
            );
        }
    }

    let insert = blend(graphite, palette.grip, 0.25);
    let inlay = [
        [-0.165, -0.010, 0.004],
        [-0.148, -0.015, 0.008],
        [-0.112, -0.015, 0.007],
        [-0.073, -0.011, 0.008],
        [-0.046, -0.005, 0.009],
    ];
    for pair in inlay.windows(2) {
        let [[low, left, right], [high, next_left, next_right]] = [pair[0], pair[1]];
        double_face(
            model,
            [
                grip_front(left, low, 0.0005),
                grip_front(right, low, 0.0005),
                grip_front(next_right, high, 0.0005),
                grip_front(next_left, high, 0.0005),
            ],
            [insert, graphite, graphite, insert],
        );
    }
    for y in [-0.151, -0.130, -0.109, -0.088, -0.067] {
        double_face(
            model,
            [
                grip_front(-0.013, y, 0.0009),
                grip_front(0.006, y - 0.004, 0.0009),
                grip_front(0.006, y - 0.001, 0.0009),
                grip_front(-0.013, y + 0.003, 0.0009),
            ],
            [[29, 34, 39], graphite, graphite, [29, 34, 39]],
        );
    }
    fastener(model, Vec2::new(0.006, -0.030), tang);
    fastener(model, Vec2::new(0.006, -0.161), tang);
}

fn grip_front(x: f32, y: f32, offset: f32) -> Vec3 {
    let rows = GRIP
        .windows(2)
        .find(|rows| y >= rows[0][0] && y <= rows[1][0])
        .unwrap_or(&GRIP[..2]);
    let blend = (y - rows[0][0]) / (rows[1][0] - rows[0][0]);
    let depth = rows[0][3] + (rows[1][3] - rows[0][3]) * blend;
    Vec3::new(x, y, depth + offset)
}

fn fastener(model: &mut Model, center: Vec2, tang: Rgb) {
    let radius = 0.0047;
    let point = |side: usize| {
        let angle = std::f32::consts::TAU * side as f32 / 8.0;
        center + Vec2::new(angle.cos(), angle.sin()) * radius
    };
    let center_point = grip_front(center.x, center.y, 0.0013);
    for side in 0..8 {
        let a = point(side);
        let b = point(side + 1);
        let tone = if side < 4 {
            blend(tang, [180, 192, 200], 0.58)
        } else {
            tang
        };
        let vertices = [
            center_point,
            grip_front(a.x, a.y, 0.001),
            grip_front(b.x, b.y, 0.001),
        ];
        triangle(model, vertices, [tone; 3]);
        triangle(
            model,
            [
                reflect(vertices[0]),
                reflect(vertices[2]),
                reflect(vertices[1]),
            ],
            [tone; 3],
        );
    }
    double_face(
        model,
        [
            grip_front(center.x - 0.0028, center.y - 0.0009, 0.0015),
            grip_front(center.x + 0.0028, center.y - 0.0009, 0.0015),
            grip_front(center.x + 0.0028, center.y + 0.0009, 0.0015),
            grip_front(center.x - 0.0028, center.y + 0.0009, 0.0015),
        ],
        [[28, 32, 36]; 4],
    );
}

fn guard(model: &mut Model, palette: &KnifePalette) {
    let outline = [
        Vec2::new(-0.045, -0.004),
        Vec2::new(0.033, -0.004),
        Vec2::new(0.041, 0.004),
        Vec2::new(0.038, 0.015),
        Vec2::new(0.027, 0.021),
        Vec2::new(-0.045, 0.023),
        Vec2::new(-0.056, 0.015),
        Vec2::new(-0.056, 0.004),
    ];
    let center = Vec2::new(-0.005, 0.008);
    let outer = outline.map(|p| p.extend(0.020));
    let inner = outline.map(|p| p.lerp(center, 0.21).extend(0.026));
    let face = blend(palette.handle, [61, 70, 78], 0.56);
    let chamfer = blend(face, palette.blade, 0.38);
    for index in 0..outline.len() {
        let next = (index + 1) % outline.len();
        double_face(
            model,
            [outer[index], outer[next], inner[next], inner[index]],
            [chamfer, chamfer, face, face],
        );
        let cap = [center.extend(0.026), inner[index], inner[next]];
        triangle(model, cap, [face; 3]);
        triangle(
            model,
            [reflect(cap[0]), reflect(cap[2]), reflect(cap[1])],
            [face; 3],
        );
        quad(
            model,
            [
                reflect(outer[index]),
                reflect(outer[next]),
                outer[next],
                outer[index],
            ],
            [face; 4],
        );
    }
}

fn reflect(point: Vec3) -> Vec3 {
    Vec3::new(point.x, point.y, -point.z)
}

fn double_face(model: &mut Model, points: [Vec3; 4], [a, b, c, d]: [Rgb; 4]) {
    quad(model, points, [a, b, c, d]);
    let [p, q, r, s] = points.map(reflect);
    quad(model, [p, s, r, q], [a, d, c, b]);
}

fn quad(model: &mut Model, [a, b, c, d]: [Vec3; 4], [u, v, w, x]: [Rgb; 4]) {
    triangle(model, [a, b, c], [u, v, w]);
    triangle(model, [a, c, d], [u, w, x]);
}

fn triangle(model: &mut Model, points: [Vec3; 3], colors: [Rgb; 3]) {
    let Some(normal) = (points[1] - points[0])
        .cross(points[2] - points[0])
        .try_normalize()
    else {
        return;
    };
    let key = Vec3::new(-0.40, 0.62, 0.68).normalize();
    let fill = Vec3::new(0.79, 0.29, -0.54).normalize();
    let shade = 0.64 + 0.29 * normal.dot(key).max(0.0) + 0.07 * normal.dot(fill).max(0.0);
    model.positions.extend(points.map(|point| point.to_array()));
    model.normals.extend([normal.to_array(); 3]);
    model.colors.extend(colors.map(|[r, g, b]| {
        let mut color = Color::srgb_u8(r, g, b).to_linear().to_f32_array();
        for channel in &mut color[..3] {
            *channel *= shade;
        }
        color
    }));
}

fn blend(a: Rgb, b: Rgb, amount: f32) -> Rgb {
    std::array::from_fn(|channel| {
        let a = f32::from(a[channel]);
        (a + (f32::from(b[channel]) - a) * amount).round() as u8
    })
}
