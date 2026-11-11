use std::{
    collections::{BTreeMap, VecDeque},
    f32::consts::{PI, TAU},
};

use bevy_math::Vec3;

use crate::network::{PlayerInfo, PlayerState};

const MAX_SAMPLES: usize = 12;
const OFFSET_WINDOW: usize = 90;
const LEGACY_DELAY_MS: f64 = 100.0;
const MAX_EXTRAPOLATION_MS: f64 = 35.0;

#[derive(Clone, Copy)]
struct Sample {
    time_ms: f64,
    state: PlayerState,
    yaw_from_previous: f32,
}

#[derive(Default)]
struct ClockOffsets {
    minima: VecDeque<(u64, f64)>,
    next: u64,
}

impl ClockOffsets {
    fn push(&mut self, offset: f64) -> f64 {
        let index = self.next;
        self.next = index.wrapping_add(1);
        if self
            .minima
            .front()
            .is_some_and(|&(previous, _)| index.wrapping_sub(previous) >= OFFSET_WINDOW as u64)
        {
            self.minima.pop_front();
        }
        if !offset.is_nan() {
            // A newer, lower offset stays in the window at least as long.
            while self
                .minima
                .back()
                .is_some_and(|&(_, value)| value >= offset)
            {
                self.minima.pop_back();
            }
            self.minima.push_back((index, offset));
        }
        self.minima
            .front()
            .map_or(f64::INFINITY, |&(_, value)| value)
    }
}

#[derive(Default)]
struct PoseBuffer {
    samples: VecDeque<Sample>,
    offsets: ClockOffsets,
    offset_ms: f64,
    arrival_ms: f64,
    jitter_ms: f64,
    playhead_ms: Option<f64>,
}

impl PoseBuffer {
    fn push(&mut self, state: PlayerState, arrival_ms: f64) {
        let mut previous = self.samples.back().copied();
        let mut time_ms = state.motion.map_or(arrival_ms, |[sent, _]| f64::from(sent));
        let mut gap_ms = 0.0;
        let mut snapped = false;
        if let Some(last) = previous {
            match (last.state.motion, state.motion) {
                (Some([old_time, old_epoch]), Some([time, epoch])) => {
                    let delta = time.wrapping_sub(old_time);
                    if delta == 0 || delta > u32::MAX / 2 {
                        return;
                    }
                    if delta <= 10_000 {
                        gap_ms = f64::from(delta);
                        time_ms = last.time_ms + gap_ms;
                        snapped = epoch != old_epoch;
                        let jitter = (arrival_ms - self.arrival_ms - gap_ms).abs().min(120.0);
                        self.jitter_ms = 0.9 * self.jitter_ms + 0.1 * jitter;
                    } else {
                        previous = None;
                    }
                }
                (None, None) => {
                    if state == last.state || arrival_ms <= last.time_ms {
                        return;
                    }
                    gap_ms = arrival_ms - last.time_ms;
                }
                _ => previous = None,
            }
        }
        if previous.is_none() {
            *self = Self::default();
        }
        if state.motion.is_some() {
            self.offset_ms = self.offsets.push(arrival_ms - time_ms);
        }
        if let Some(last) = previous {
            let limit = (gap_ms * 15.0).max(256.0) as f32;
            let displacement =
                Vec3::from_array(state.position) - Vec3::from_array(last.state.position);
            if snapped
                || state.elapsed + 0.05 < last.state.elapsed
                || displacement.length_squared() > limit * limit
            {
                self.samples.clear();
                self.playhead_ms = None;
            }
        }
        self.arrival_ms = arrival_ms;
        if self.samples.len() == MAX_SAMPLES {
            self.samples.pop_front();
        }
        let yaw_from_previous = self.samples.back().map_or(0.0, |previous| {
            (state.yaw - previous.state.yaw + PI).rem_euclid(TAU) - PI
        });
        self.samples.push_back(Sample {
            time_ms,
            state,
            yaw_from_previous,
        });
    }

    fn sample(&mut self, now_ms: f64) -> Option<PlayerState> {
        let delay = if self.samples.back()?.state.motion.is_some() {
            (40.0 + self.jitter_ms * 2.0).min(95.0)
        } else {
            LEGACY_DELAY_MS
        };
        let target = now_ms - self.offset_ms - delay;
        let at = self
            .playhead_ms
            .map_or(target, |playhead| playhead.max(target));
        self.playhead_ms = Some(at);
        while self.samples.len() > 2 && self.samples[1].time_ms <= at {
            self.samples.pop_front();
        }
        let first = &self.samples[0];
        if at <= first.time_ms {
            return Some(first.state);
        }
        if let Some(second) = self.samples.get(1).filter(|sample| at <= sample.time_ms) {
            let fraction = (at - first.time_ms) / (second.time_ms - first.time_ms);
            return Some(interpolate(first, second, fraction));
        }
        let latest = self
            .samples
            .back()
            .expect("A buffered pose was checked above");
        let mut state = latest.state;
        if state.motion.is_some() && !state.finished && self.samples.len() >= 2 {
            let previous = &self.samples[self.samples.len() - 2];
            let gap = latest.time_ms - previous.time_ms;
            if (1.0..=250.0).contains(&gap) {
                let extra = (at - latest.time_ms).clamp(0.0, MAX_EXTRAPOLATION_MS);
                let position = Vec3::from_array(state.position);
                let displacement = position - Vec3::from_array(previous.state.position);
                state.position = (position + displacement * (extra / gap) as f32).to_array();
            }
        }
        Some(state)
    }
}

fn interpolate(first: &Sample, second: &Sample, fraction: f64) -> PlayerState {
    let t = fraction as f32;
    let yaw_delta = second.yaw_from_previous;
    let (first, second) = (&first.state, &second.state);
    let mut state = if t < 1.0 { *first } else { *second };
    state.position = Vec3::from_array(first.position)
        .lerp(Vec3::from_array(second.position), t)
        .to_array();
    state.yaw = first.yaw + yaw_delta * t;
    state.pitch = first.pitch + (second.pitch - first.pitch) * t;
    let eye = first.camera_eye_height();
    state.eye_height = Some(eye + (second.camera_eye_height() - eye) * t);
    if first.elapsed > 0.0 && !first.finished && second.elapsed >= first.elapsed {
        state.elapsed = first.elapsed + (second.elapsed - first.elapsed) * fraction;
    }
    state
}

#[derive(Default)]
struct RemotePlayer {
    buffer: PoseBuffer,
    shown: Option<PlayerState>,
    roster: u64,
}

/// Keeps one pose per player for the camera, map effects and avatar to share in a frame.
#[derive(Default)]
pub struct RoomPoses {
    players: BTreeMap<u64, RemotePlayer>,
    roster: u64,
}

impl RoomPoses {
    pub fn ingest(&mut self, players: &[PlayerInfo], local: Option<u64>, arrival_ms: f64) {
        self.roster = self.roster.wrapping_add(1);
        for player in players {
            if Some(player.id) != local
                && player.spectating.is_none()
                && let Some(state) = player.state
            {
                let remote = self.players.entry(player.id).or_default();
                remote.roster = self.roster;
                remote.buffer.push(state, arrival_ms);
            }
        }
        self.players
            .retain(|_, player| player.roster == self.roster);
    }

    pub fn sample(&mut self, now_ms: f64) {
        for player in self.players.values_mut() {
            player.shown = player.buffer.sample(now_ms);
        }
    }

    pub fn get(&self, id: u64) -> Option<PlayerState> {
        self.players.get(&id)?.shown
    }

    pub fn clear(&mut self) {
        self.players.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(x: f32, motion: Option<[u32; 2]>) -> PlayerState {
        PlayerState {
            position: [x, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            elapsed: 2.0,
            finished: false,
            crouched: false,
            eye_height: None,
            motion,
        }
    }

    #[test]
    fn rolling_clock_minimum_expires_old_samples_across_counter_wrap() {
        let mut offsets = ClockOffsets {
            next: u64::MAX - 12,
            ..ClockOffsets::default()
        };
        let mut history = VecDeque::new();
        for index in 0..(OFFSET_WINDOW * 4) {
            let sample = match index {
                0 => -500.0,
                8 => -400.0,
                10 | 91 => f64::NAN,
                _ => ((index * 613) % 99) as f64,
            };
            history.push_back(sample);
            if history.len() > OFFSET_WINDOW {
                history.pop_front();
            }
            assert_eq!(
                offsets.push(sample),
                history.iter().copied().fold(f64::INFINITY, f64::min)
            );
        }
    }

    #[test]
    fn blends_pose_and_eye_height_over_the_short_yaw_arc() {
        let mut buffer = PoseBuffer::default();
        let mut standing = pose(0.0, Some([0, 1]));
        standing.yaw = 170.0_f32.to_radians();
        let mut crouching = pose(50.0, Some([50, 1]));
        crouching.yaw = (-170.0_f32).to_radians();
        crouching.crouched = true;
        crouching.pitch = -0.5;
        buffer.push(standing, 10.0);
        buffer.push(crouching, 60.0);
        let midpoint = buffer.sample(75.0).unwrap();
        assert_eq!(midpoint.position, [25.0, 0.0, 0.0]);
        assert!((midpoint.yaw - PI).abs() < 1e-6);
        assert_eq!(midpoint.camera_eye_height(), 55.5);
        assert_eq!(midpoint.pitch, -0.25);
    }

    #[test]
    fn snaps_short_teleports_and_caps_prediction_when_updates_stop() {
        let mut buffer = PoseBuffer::default();
        buffer.push(pose(10.0, Some([0, 1])), 0.0);
        buffer.push(pose(35.0, Some([50, 2])), 50.0);
        assert_eq!(buffer.sample(50.0).unwrap().position[0], 35.0);
        buffer.push(pose(85.0, Some([100, 2])), 100.0);
        assert_eq!(buffer.sample(200.0).unwrap().position[0], 120.0);
        buffer.push(pose(150.0, Some([99, 2])), 200.0);
        assert_eq!(buffer.sample(400.0).unwrap().position[0], 120.0);
    }

    #[test]
    fn handles_legacy_peers_and_wrapping_sender_clocks() {
        let mut legacy = PoseBuffer::default();
        legacy.push(pose(0.0, None), 0.0);
        legacy.push(pose(50.0, None), 50.0);
        assert_eq!(legacy.sample(125.0).unwrap().position[0], 25.0);
        assert_eq!(legacy.sample(500.0).unwrap().position[0], 50.0);
        let mut native = PoseBuffer::default();
        native.push(pose(0.0, Some([u32::MAX - 20, 1])), 0.0);
        native.push(pose(50.0, Some([29, 1])), 50.0);
        assert_eq!(native.sample(65.0).unwrap().position[0], 25.0);
    }
}
