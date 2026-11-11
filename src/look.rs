use crate::movement::{normalize_source_yaw, source_yaw_to_native};
use bevy_math::Vec2;

#[derive(Clone, Copy)]
pub struct SourceLook {
    pub yaw: f64,
    pitch: f64,
}

impl SourceLook {
    pub fn new(yaw: f64, bevy_pitch: f32) -> Self {
        Self {
            yaw: normalize_source_yaw(yaw),
            pitch: -f64::from(bevy_pitch).to_degrees(),
        }
    }

    pub fn native(self) -> (f32, f32) {
        (
            source_yaw_to_native(self.yaw),
            -self.pitch.to_radians() as f32,
        )
    }

    pub fn apply_mouse(&mut self, delta: Vec2, degrees_per_count: f64, invert: bool) {
        if !delta.is_finite() || delta == Vec2::ZERO {
            return;
        }
        self.turn(-f64::from(delta.x) * degrees_per_count);
        let vertical = f64::from(delta.y) * degrees_per_count;
        self.pitch = (self.pitch + if invert { -vertical } else { vertical }).clamp(-89.0, 89.0);
    }

    pub fn apply_input(
        &mut self,
        relative: &[Vec2],
        cursor: Vec2,
        scale: f32,
        degrees_per_count: f64,
        invert: bool,
    ) {
        if relative.is_empty() {
            self.apply_mouse(cursor * scale, degrees_per_count, invert);
        } else {
            for &delta in relative {
                self.apply_mouse(delta, degrees_per_count, invert);
            }
        }
    }

    pub fn turn(&mut self, degrees: f64) {
        self.yaw = normalize_source_yaw(self.yaw + degrees);
    }

    pub fn set_native_pitch(&mut self, pitch: f32) {
        self.pitch = -f64::from(pitch).to_degrees();
    }
}

#[derive(Default)]
pub struct CursorMotion {
    position: Option<Vec2>,
    warp_target: Option<Vec2>,
}

impl CursorMotion {
    pub fn observe(&mut self, position: Vec2) -> Vec2 {
        let previous = self.position.replace(position);
        if self
            .warp_target
            .is_some_and(|target| target.distance_squared(position) <= 1.0)
        {
            self.warp_target = None;
            return Vec2::ZERO;
        }
        previous.map_or(Vec2::ZERO, |previous| position - previous)
    }

    pub fn recenter(&mut self, center: Vec2) {
        self.position = Some(center);
        self.warp_target = Some(center);
    }

    pub fn near_edge(&self, size: Vec2) -> bool {
        let margin = (size * 0.15).min(Vec2::splat(64.0));
        self.position.is_some_and(|position| {
            position.cmplt(margin).any() || position.cmpgt(size - margin).any()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_motion_survives_without_a_raw_device_and_does_not_double_count() {
        let mut cursor = CursorMotion::default();
        let center = Vec2::new(600.0, 400.0);
        cursor.recenter(center);
        assert_eq!(cursor.observe(center), Vec2::ZERO);
        let pointer = cursor.observe(center + Vec2::new(12.0, -5.0));
        let mut fallback = SourceLook::new(0.0, 0.0);
        fallback.apply_input(&[], pointer, 1.5, 1.0, false);
        assert_eq!(fallback.yaw, -18.0);
        let mut raw = SourceLook::new(0.0, 0.0);
        raw.apply_input(&[Vec2::X], pointer, 1.5, 1.0, false);
        assert_eq!(raw.yaw, -1.0);
        cursor.recenter(center);
        assert_eq!(cursor.observe(center), Vec2::ZERO);
    }

    #[test]
    fn mouse_angles_keep_source_precision_and_pitch_limit() {
        let mut look = SourceLook::new(179.95, 0.0);
        look.apply_mouse(Vec2::new(-2.0, 2000.0), 0.022 * 2.5, false);
        assert!((look.yaw + 179.94).abs() < 1e-12);
        assert_eq!(look.pitch, 89.0);
        look.apply_mouse(Vec2::new(0.0, 100.0), 0.022 * 2.5, true);
        assert_eq!(look.pitch, 83.5);

        let mut batched = SourceLook::new(0.0, 0.0);
        batched.apply_input(
            &[Vec2::new(0.0, 100.0), Vec2::new(0.0, -5.0)],
            Vec2::ZERO,
            1.0,
            1.0,
            false,
        );
        assert_eq!(batched.pitch, 84.0);
    }
}
