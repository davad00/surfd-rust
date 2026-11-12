use bevy::{
    color::ColorToComponents,
    prelude::{Color, Vec2, Vec3},
};

use super::Model;

const SLEEVE: [u8; 3] = [53, 63, 70];
const CUFF: [u8; 3] = [43, 49, 53];
const CUFF_EDGE: [u8; 3] = [59, 67, 72];
const GLOVE: [u8; 3] = [49, 54, 58];
const FLEX: [u8; 3] = [42, 47, 50];
const PADDING: [u8; 3] = [64, 70, 74];

pub(super) fn add_hand(model: &mut Model) {
    // The sleeve and palm share one continuous surface under the knuckle padding.
    let arm = [
        Section::new(0.264, -0.473, 0.027, 0.071, 0.056, SLEEVE),
        Section::new(0.207, -0.384, 0.026, 0.065, 0.053, SLEEVE),
        Section::new(0.155, -0.305, 0.022, 0.057, 0.047, SLEEVE),
        Section::new(0.112, -0.232, 0.018, 0.049, 0.041, SLEEVE),
        Section::new(0.090, -0.193, 0.015, 0.044, 0.036, SLEEVE),
        Section::new(0.085, -0.183, 0.014, 0.047, 0.039, CUFF_EDGE),
        Section::new(0.080, -0.176, 0.013, 0.048, 0.040, CUFF),
        Section::new(0.074, -0.153, 0.011, 0.045, 0.038, CUFF),
        Section::new(0.073, -0.147, 0.011, 0.046, 0.039, CUFF_EDGE),
        Section::new(0.071, -0.139, 0.012, 0.044, 0.038, GLOVE),
        Section::new(0.068, -0.111, 0.013, 0.052, 0.043, GLOVE),
        Section::new(0.066, -0.084, 0.010, 0.054, 0.045, GLOVE),
        Section::new(0.063, -0.057, 0.008, 0.048, 0.044, GLOVE),
        Section::new(0.056, -0.033, 0.003, 0.037, 0.035, GLOVE),
        Section::new(0.046, -0.016, -0.001, 0.019, 0.020, GLOVE),
        Section::new(0.042, -0.010, -0.002, 0.009, 0.011, FLEX),
    ];
    loft::<12>(model, &arm, Vec3::Z);

    let knuckle_pad = [
        Section::new(0.084, -0.121, 0.039, 0.012, 0.006, FLEX),
        Section::new(0.089, -0.111, 0.044, 0.017, 0.009, GLOVE),
        Section::new(0.092, -0.090, 0.048, 0.018, 0.010, PADDING),
        Section::new(0.089, -0.067, 0.048, 0.017, 0.009, PADDING),
        Section::new(0.081, -0.049, 0.045, 0.013, 0.007, GLOVE),
        Section::new(0.075, -0.043, 0.040, 0.008, 0.005, FLEX),
    ];
    loft::<10>(model, &knuckle_pad, Vec3::Z);

    // The fingers turn around the near face of the grip and close on its far edge.
    for (height, reach, thickness) in [
        (-0.042, 1.00, 1.00),
        (-0.068, 1.04, 1.00),
        (-0.093, 1.00, 0.94),
        (-0.117, 0.93, 0.84),
    ] {
        let finger = [
            Section::new(0.076, 0.000, 0.023, 0.012, 0.013, GLOVE),
            Section::new(0.066, 0.003, 0.045, 0.013, 0.014, PADDING),
            Section::new(0.046, 0.001, 0.052, 0.012, 0.013, PADDING),
            Section::new(0.025, -0.002, 0.049, 0.012, 0.011, FLEX),
            Section::new(0.004, -0.004, 0.045, 0.011, 0.011, GLOVE),
            Section::new(-0.015, -0.006, 0.039, 0.010, 0.010, GLOVE),
            Section::new(-0.027, -0.008, 0.026, 0.010, 0.009, GLOVE),
            Section::new(-0.031, -0.008, 0.014, 0.007, 0.006, FLEX),
            Section::new(-0.027, -0.008, 0.009, 0.004, 0.004, FLEX),
        ]
        .map(|mut section| {
            section.center.x = 0.076 + (section.center.x - 0.076) * reach;
            section.center.y += height;
            section.radius *= thickness;
            section
        });
        loft::<8>(model, &finger, Vec3::Y);
    }

    let thumb = [
        Section::new(0.082, -0.056, 0.012, 0.020, 0.017, GLOVE),
        Section::new(0.073, -0.036, 0.027, 0.018, 0.017, GLOVE),
        Section::new(0.059, -0.025, 0.042, 0.015, 0.015, PADDING),
        Section::new(0.042, -0.018, 0.043, 0.014, 0.012, GLOVE),
        Section::new(0.025, -0.020, 0.036, 0.011, 0.010, FLEX),
        Section::new(0.019, -0.024, 0.030, 0.006, 0.006, FLEX),
    ];
    loft::<10>(model, &thumb, Vec3::Y);
}

#[derive(Clone, Copy)]
struct Section {
    center: Vec3,
    radius: Vec2,
    color: [u8; 3],
}

impl Section {
    fn new(x: f32, y: f32, z: f32, width: f32, depth: f32, color: [u8; 3]) -> Self {
        Self {
            center: Vec3::new(x, y, z),
            radius: Vec2::new(width, depth),
            color,
        }
    }
}

#[derive(Clone, Copy)]
struct Vertex {
    position: Vec3,
    normal: Vec3,
    color: [f32; 4],
}

fn loft<const SIDES: usize>(model: &mut Model, sections: &[Section], upright: Vec3) {
    let light = Vec3::new(-0.37, 0.73, 0.56).normalize();
    let last = sections.len() - 1;
    let rings: Vec<[Vertex; SIDES]> = sections
        .iter()
        .enumerate()
        .map(|(index, section)| {
            let previous = sections[index.saturating_sub(1)];
            let next = sections[(index + 1).min(last)];
            let span = next.center - previous.center;
            let axis = span.normalize();
            let right = upright.cross(axis).normalize();
            let depth = axis.cross(right);
            let slope = (next.radius - previous.radius) / span.length();
            let [red, green, blue] = section.color;
            let base = Color::srgb_u8(red, green, blue).to_linear().to_f32_array();
            std::array::from_fn(|side| {
                let angle = std::f32::consts::TAU * side as f32 / SIDES as f32;
                let (sin, cos) = angle.sin_cos();
                let position = section.center
                    + right * (cos * section.radius.x)
                    + depth * (sin * section.radius.y);
                let normal = (right * (cos / section.radius.x) + depth * (sin / section.radius.y))
                    .normalize();
                let taper = cos * cos * slope.x + sin * sin * slope.y;
                let normal = (normal - axis * taper).normalize();
                let facing = normal.dot(light).max(0.0);
                let illumination = 0.58 + 0.39 * facing + 0.07 * facing.powi(8);
                Vertex {
                    position,
                    normal,
                    color: [
                        base[0] * illumination,
                        base[1] * illumination,
                        base[2] * illumination,
                        1.0,
                    ],
                }
            })
        })
        .collect();

    for strip in rings.windows(2) {
        let [first, second] = strip else { continue };
        for side in 0..SIDES {
            let next = (side + 1) % SIDES;
            add_triangle(model, [first[side], first[next], second[next]]);
            add_triangle(model, [first[side], second[next], second[side]]);
        }
    }

    for (index, direction) in [(0, -1.0), (last, 1.0)] {
        let other = if index == 0 { 1 } else { last - 1 };
        let outward = (sections[index].center - sections[other].center).normalize();
        for side in 0..SIDES {
            let next = (side + 1) % SIDES;
            let a = rings[index][side];
            let b = rings[index][next];
            let center = Vertex {
                position: sections[index].center,
                normal: outward,
                color: a.color,
            };
            let [a, b] = if direction < 0.0 { [b, a] } else { [a, b] };
            add_triangle(model, [center, a, b]);
        }
    }
}

fn add_triangle(model: &mut Model, vertices: [Vertex; 3]) {
    model
        .positions
        .extend(vertices.map(|vertex| vertex.position.to_array()));
    model
        .normals
        .extend(vertices.map(|vertex| vertex.normal.to_array()));
    model.colors.extend(vertices.map(|vertex| vertex.color));
}
