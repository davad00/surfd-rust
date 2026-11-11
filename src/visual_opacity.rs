use bevy_math::{DQuat, DVec3, Vec3};
use serde::Deserialize;

pub enum VisualOpacity {
    AreaWindow(AreaWindow),
    Proximity(Proximity),
}

impl VisualOpacity {
    pub fn factor(&self, eye: Vec3, actor_center: Vec3, fov_degrees: f64) -> f32 {
        let factor = match self {
            Self::AreaWindow(window) => window.factor(source_axes(eye), fov_degrees),
            Self::Proximity(proximity) => proximity.factor(source_axes(actor_center)),
        };
        factor as f32
    }
}

fn source_axes(position: Vec3) -> DVec3 {
    DVec3::new(position.x.into(), (-position.z).into(), position.y.into())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AreaWindowData {
    origin: [f64; 3],
    angles: [f64; 3],
    min: [f64; 3],
    max: [f64; 3],
    fade_start: f64,
    fade_end: f64,
    minimum_opacity: f64,
}

pub struct AreaWindow {
    origin: DVec3,
    inverse_rotation: DQuat,
    min: DVec3,
    max: DVec3,
    fade_start: f64,
    fade_end: f64,
    minimum_opacity: f64,
}

impl From<&AreaWindowData> for AreaWindow {
    fn from(data: &AreaWindowData) -> Self {
        let [pitch, yaw, roll] = data.angles.map(f64::to_radians);
        let rotation = DQuat::from_rotation_z(yaw)
            * DQuat::from_rotation_y(pitch)
            * DQuat::from_rotation_x(roll);
        Self {
            origin: DVec3::from_array(data.origin),
            inverse_rotation: rotation.conjugate(),
            min: DVec3::from_array(data.min),
            max: DVec3::from_array(data.max),
            fade_start: data.fade_start,
            fade_end: data.fade_end,
            minimum_opacity: data.minimum_opacity,
        }
    }
}

impl AreaWindow {
    fn factor(&self, eye: DVec3, fov_degrees: f64) -> f64 {
        let local = self.inverse_rotation * (eye - self.origin);
        let distance = (self.min - local)
            .max(local - self.max)
            .max(DVec3::ZERO)
            .length()
            * fov_degrees
            / 90.0;
        let fade = if self.fade_start == self.fade_end {
            if distance >= self.fade_end { 1.0 } else { 0.0 }
        } else {
            ((distance - self.fade_start) / (self.fade_end - self.fade_start)).clamp(0.0, 1.0)
        };
        self.minimum_opacity + (1.0 - self.minimum_opacity) * fade
    }
}

#[derive(Clone, Deserialize)]
pub struct Proximity {
    center: [f64; 3],
    distance: Vec<f64>,
    alpha: Vec<f64>,
}

impl Proximity {
    pub(crate) fn valid(&self) -> bool {
        !self.distance.is_empty()
            && self.alpha.len() == self.distance.len()
            && self.distance.windows(2).all(|pair| pair[0] < pair[1])
    }

    fn factor(&self, position: DVec3) -> f64 {
        let distance = position.distance(DVec3::from_array(self.center));
        let next = self.distance.partition_point(|point| *point <= distance);
        if next == 0 {
            return self.alpha[0];
        }
        if next == self.distance.len() {
            return self.alpha[next - 1];
        }
        let previous = next - 1;
        let blend =
            (distance - self.distance[previous]) / (self.distance[next] - self.distance[previous]);
        self.alpha[previous] + (self.alpha[next] - self.alpha[previous]) * blend
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_uses_oriented_bounds_and_reference_fov_scale() {
        let window = AreaWindow::from(&AreaWindowData {
            origin: [0.0; 3],
            angles: [0.0, 90.0, 0.0],
            min: [-10.0, -2.0, -1.0],
            max: [10.0, 2.0, 1.0],
            fade_start: 1.0,
            fade_end: 5.0,
            minimum_opacity: 0.2,
        });
        let eye = DVec3::new(5.0, 8.0, 0.0);
        assert!((window.factor(eye, 90.0) - 0.6).abs() < 1e-12);
        assert!((window.factor(eye, 60.0) - 0.4).abs() < 1e-12);
    }

    #[test]
    fn proximity_uses_player_center_and_interpolates_sampled_alpha() {
        let proximity = VisualOpacity::Proximity(Proximity {
            center: [2.0, 3.0, 4.0],
            distance: vec![0.0, 10.0, 20.0],
            alpha: vec![1.0, 0.8, 0.0],
        });
        assert_eq!(
            proximity.factor(Vec3::ZERO, Vec3::new(17.0, 4.0, -3.0), 90.0),
            0.4
        );
        assert_eq!(
            proximity.factor(Vec3::ZERO, Vec3::new(27.0, 4.0, -3.0), 90.0),
            0.0
        );
    }
}
