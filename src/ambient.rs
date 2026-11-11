use std::{
    f64::consts::TAU,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

const SAMPLE_RATE: usize = 48_000;
const LOOP_START: usize = SAMPLE_RATE / 4;
const SUSPEND_SAMPLES: usize = SAMPLE_RATE * 350 / 1000;
const LOOKAHEAD_SAMPLES: usize = SAMPLE_RATE * 6 / 1000;
const QUIET_KNEE_AMPLITUDE: f64 = 0.063_095_734_448_019_33;

/// The procedural ambience in the local Surfd client uses a seeded,
/// cross-faded mono buffer. Keep the seed and JS Float32Array rounding.
fn wind_buffer() -> Arc<[f32]> {
    let mut samples = vec![0.0; SAMPLE_RATE * 12];
    let mut seed = 19_347u32;
    let mut brown = 0.0;
    for sample in &mut samples {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let white = f64::from(seed) / 2_147_483_648.0 - 1.0;
        brown = (brown + white * 0.035) / 1.015;
        *sample = (brown * 3.0) as f32;
    }
    let tail = samples.len() - LOOP_START;
    for index in 0..LOOP_START {
        let progress = index as f64 / LOOP_START as f64;
        samples[tail + index] = (f64::from(samples[tail + index]) * (1.0 - progress)
            + f64::from(samples[index]) * progress) as f32;
    }
    samples.into()
}

struct WindControls {
    active: AtomicBool,
    gain: AtomicU32,
    cutoff: AtomicU32,
}

impl Default for WindControls {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(false),
            gain: AtomicU32::new(0.0f32.to_bits()),
            cutoff: AtomicU32::new(800.0f32.to_bits()),
        }
    }
}

impl WindControls {
    fn configure(&self, active: bool, enabled: bool, volume: f32, cutoff: f32) {
        let volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            0.14
        };
        let gain = if active && enabled {
            volume * 0.22
        } else {
            0.0
        };
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
        self.cutoff.store(cutoff.to_bits(), Ordering::Relaxed);
        self.active.store(active, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
struct FilterCoefficients {
    b0: f64,
    b1: f64,
    a1: f64,
    a2: f64,
}

impl FilterCoefficients {
    fn new(cutoff: f64, q_db: f64, high_pass: bool) -> Self {
        let (sine, cosine) = (TAU * cutoff / SAMPLE_RATE as f64).sin_cos();
        let alpha = sine / (2.0 * 10f64.powf(q_db / 20.0));
        let inverse_a0 = (1.0 + alpha).recip();
        let (b0, b1) = if high_pass {
            let numerator = 1.0 + cosine;
            (numerator * 0.5, -numerator)
        } else {
            let numerator = 1.0 - cosine;
            (numerator * 0.5, numerator)
        };
        Self {
            b0: b0 * inverse_a0,
            b1: b1 * inverse_a0,
            a1: -2.0 * cosine * inverse_a0,
            a2: (1.0 - alpha) * inverse_a0,
        }
    }
}

#[derive(Default)]
struct FilterHistory {
    input: [f64; 2],
    output: [f64; 2],
}

impl FilterHistory {
    fn sample(&mut self, sample: f64, coefficients: FilterCoefficients) -> f64 {
        let result = coefficients.b0 * (sample + self.input[1]) + coefficients.b1 * self.input[0]
            - coefficients.a1 * self.output[0]
            - coefficients.a2 * self.output[1];
        self.input = [sample, self.input[0]];
        self.output = [result, self.output[0]];
        result
    }
}

struct SoftCompressor {
    gain: f64,
    makeup: f64,
    attack: f64,
    release: f64,
    delay: [f64; LOOKAHEAD_SAMPLES],
    cursor: usize,
}

impl SoftCompressor {
    fn new() -> Self {
        // The E7 browser compressor uses a -18 dB threshold, 12 dB knee,
        // and 4:1 ratio. Use a quadratic soft knee with the same nominal
        // settings and lookahead.
        Self {
            gain: 1.0,
            makeup: 10f64.powf(8.1 / 20.0),
            attack: smoothing(0.003),
            release: smoothing(0.25),
            delay: [0.0; LOOKAHEAD_SAMPLES],
            cursor: 0,
        }
    }

    fn sample(&mut self, sample: f64) -> f32 {
        let level = sample.abs();
        let target_gain = if level > QUIET_KNEE_AMPLITUDE {
            let input_db = 20.0 * level.log10();
            let reduction = if input_db < -12.0 {
                -0.75 * (input_db + 24.0).powi(2) / 24.0
            } else {
                -0.75 * (input_db + 18.0)
            };
            10f64.powf(reduction / 20.0)
        } else {
            1.0
        };
        let smoothing = if target_gain < self.gain {
            self.attack
        } else {
            self.release
        };
        self.gain += (target_gain - self.gain) * smoothing;
        let delayed = self.delay[self.cursor];
        self.delay[self.cursor] = sample;
        self.cursor = (self.cursor + 1) % self.delay.len();
        (delayed * self.gain * self.makeup).clamp(-1.0, 1.0) as f32
    }
}

fn smoothing(seconds: f64) -> f64 {
    -(-1.0 / (seconds * SAMPLE_RATE as f64)).exp_m1()
}

struct WindStream {
    controls: Arc<WindControls>,
    noise: Arc<[f32]>,
    cursor: usize,
    inactive_samples: usize,
    started: bool,
    high_pass: FilterCoefficients,
    high_history: FilterHistory,
    air_history: FilterHistory,
    cutoff: f64,
    cutoff_smoothing: f64,
    master: f64,
    master_smoothing: f64,
    phase: f64,
    compressor: SoftCompressor,
}

impl WindStream {
    fn new(noise: Arc<[f32]>, controls: Arc<WindControls>) -> Self {
        Self {
            controls,
            noise,
            cursor: 0,
            inactive_samples: SUSPEND_SAMPLES,
            started: false,
            high_pass: FilterCoefficients::new(110.0, 1.0, true),
            high_history: FilterHistory::default(),
            air_history: FilterHistory::default(),
            cutoff: 680.0,
            cutoff_smoothing: smoothing(1.5),
            master: 0.0,
            master_smoothing: smoothing(0.075),
            phase: 0.0,
            compressor: SoftCompressor::new(),
        }
    }
}

impl Iterator for WindStream {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let active = self.controls.active.load(Ordering::Acquire);
        let target_gain = f32::from_bits(self.controls.gain.load(Ordering::Relaxed));
        if active && (self.started || target_gain > 0.0) {
            self.started = true;
            self.inactive_samples = 0;
        } else if self.inactive_samples == SUSPEND_SAMPLES {
            return Some(0.0);
        } else {
            self.inactive_samples += 1;
        }

        let target_cutoff = f32::from_bits(self.controls.cutoff.load(Ordering::Relaxed));
        self.cutoff += (f64::from(target_cutoff) - self.cutoff) * self.cutoff_smoothing;
        self.master += (f64::from(target_gain) - self.master) * self.master_smoothing;
        let air_filter = FilterCoefficients::new(self.cutoff, 0.3, false);
        let high = self
            .high_history
            .sample(f64::from(self.noise[self.cursor]), self.high_pass);
        let air = self.air_history.sample(high, air_filter);
        self.cursor += 1;
        if self.cursor == self.noise.len() {
            self.cursor = LOOP_START;
        }
        let gust = 0.45 + 0.07 * self.phase.sin();
        self.phase += 0.067 * TAU / SAMPLE_RATE as f64;
        if self.phase >= TAU {
            self.phase -= TAU;
        }
        Some(self.compressor.sample(air * gust * self.master))
    }
}

#[cfg(feature = "client")]
mod plugin {
    use super::*;
    use bevy::{
        audio::{AddAudioSource, ChannelCount, Decodable, SampleRate, Source},
        prelude::*,
        reflect::TypePath,
        window::PrimaryWindow,
    };
    use std::time::Duration;

    use crate::{
        menu::{MenuState, Page},
        view::Game,
    };

    pub struct AmbientPlugin;

    impl Plugin for AmbientPlugin {
        fn build(&self, app: &mut App) {
            app.add_audio_source::<WindSource>()
                .init_resource::<AmbientControls>()
                .add_systems(Startup, spawn_wind)
                .add_systems(Update, configure_wind);
        }
    }

    #[derive(Resource, Default)]
    struct AmbientControls(Arc<WindControls>);

    #[derive(Asset, TypePath)]
    struct WindSource {
        samples: Arc<[f32]>,
        controls: Arc<WindControls>,
    }

    impl Decodable for WindSource {
        type Decoder = WindStream;

        fn decoder(&self) -> Self::Decoder {
            WindStream::new(Arc::clone(&self.samples), Arc::clone(&self.controls))
        }
    }

    impl Source for WindStream {
        fn current_span_len(&self) -> Option<usize> {
            None
        }

        fn channels(&self) -> ChannelCount {
            ChannelCount::new(1).unwrap()
        }

        fn sample_rate(&self) -> SampleRate {
            SampleRate::new(SAMPLE_RATE as u32).unwrap()
        }

        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    fn spawn_wind(
        mut commands: Commands,
        mut sources: ResMut<Assets<WindSource>>,
        controls: Res<AmbientControls>,
    ) {
        commands.spawn(AudioPlayer(sources.add(WindSource {
            samples: wind_buffer(),
            controls: Arc::clone(&controls.0),
        })));
    }

    fn configure_wind(
        controls: Res<AmbientControls>,
        game: Res<Game>,
        menu: Res<MenuState>,
        primary: Query<&Window, With<PrimaryWindow>>,
    ) {
        let focused = primary.single().is_ok_and(|window| window.focused);
        let active = menu.page == Page::Playing && game.pending.is_none() && focused;
        controls.0.configure(
            active,
            menu.settings.environmental_audio,
            menu.settings.environmental_volume,
            map_cutoff(&game.course.name),
        );
    }

    fn map_cutoff(map: &str) -> f32 {
        let Some(entry) = bevy_surf::catalog::find(map) else {
            return 280.0;
        };
        // The shipped Surfd map metadata groups the catalog by ambience.
        // Remaining catalog maps use Utopia, at 620 Hz.
        match entry.id.as_str() {
            "surf_boreas" | "surf_lux" | "surf_eclipse" | "surf_summer_ksf" | "surf_tendies"
            | "surf_whiteout" | "surf_summit" | "surf_frost" | "surf_andromeda" | "surf_artois"
            | "surf_listless_ksf" | "surf_sofia_fix" | "surf_ravine" | "surf_ivory"
            | "surf_leidenfrost" => 800.0,
            "surf_mesa_fixed"
            | "surf_demise"
            | "surf_nyx"
            | "surf_legends"
            | "surf_forbidden_ways_ksf"
            | "surf_nuclear"
            | "surf_fruits"
            | "surf_sandtrap2"
            | "surf_666"
            | "surf_angst"
            | "surf_bugs"
            | "surf_stonework4"
            | "surf_gigapede"
            | "surf_sinsane_ksf"
            | "surf_race_final"
            | "surf_me"
            | "surf_for_shareholder_value"
            | "surf_agony"
            | "surf_threnody"
            | "surf_fiellu_ksf"
            | "surf_overgrowth" => 430.0,
            _ => 620.0,
        }
    }
}

#[cfg(feature = "client")]
pub use plugin::AmbientPlugin;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_loop_matches_reference_buffer() {
        let noise = wind_buffer();
        assert_eq!(noise.len(), 576_000);
        for (index, expected) in [
            (0, 0.048_423_06),
            (1, -0.030_021_204),
            (2, 0.047_073_226),
            (12_000, 0.104_206_03),
            (575_999, 0.074_160_52),
        ] {
            assert!((noise[index] - expected).abs() < 1e-7, "sample {index}");
        }
    }

    #[test]
    fn wind_fades_to_silence_and_keeps_its_loop_position() {
        let controls = Arc::new(WindControls::default());
        let mut stream = WindStream::new(wind_buffer(), Arc::clone(&controls));
        assert_eq!(stream.next(), Some(0.0));
        assert_eq!(stream.cursor, 0);

        controls.configure(true, true, 0.14, 430.0);
        let samples = stream.by_ref().take(SAMPLE_RATE).collect::<Vec<_>>();
        assert!(
            samples
                .iter()
                .all(|sample| sample.is_finite() && sample.abs() < 0.15)
        );
        assert!(samples.iter().any(|sample| sample.abs() > 0.001));
        assert!((430.0..680.0).contains(&stream.cutoff));

        controls.configure(false, true, 0.14, 430.0);
        stream.by_ref().take(SUSPEND_SAMPLES).for_each(drop);
        let stopped_at = stream.cursor;
        assert_eq!(stream.next(), Some(0.0));
        assert_eq!(stream.cursor, stopped_at);
    }
}
