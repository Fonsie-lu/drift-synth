//! Envelopes and LFOs.

use crate::analog::Rng32;
use crate::osc::{sin_lut, wrap01};
use crate::params::{LfoShape, SyncDivision};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvStage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
    Finished,
}

impl EnvStage {
    pub fn as_u8(self) -> u8 {
        match self {
            EnvStage::Idle | EnvStage::Finished => 0,
            EnvStage::Attack => 1,
            EnvStage::Decay => 2,
            EnvStage::Sustain => 3,
            EnvStage::Release => 4,
        }
    }
}

/// One-pole ADSR using the target-overshoot form, so each segment traverses
/// the curved early part of an exponential while still taking exactly the time
/// the user dialled in.
#[derive(Clone, Copy, Debug)]
pub struct Adsr {
    pub out: f32,
    pub stage: EnvStage,
    a_coef: f32,
    a_base: f32,
    d_coef: f32,
    d_base: f32,
    r_coef: f32,
    r_base: f32,
    sustain: f32,
}

impl Default for Adsr {
    fn default() -> Self {
        Self {
            out: 0.0,
            stage: EnvStage::Idle,
            a_coef: 0.0,
            a_base: 1.0,
            d_coef: 0.0,
            d_base: 1.0,
            r_coef: 0.0,
            r_base: 0.0,
            sustain: 1.0,
        }
    }
}

/// Below this the release is considered finished.
pub const ENV_EPSILON: f32 = 1.0e-5;

#[inline]
fn coef(time_s: f32, ratio: f32, sample_rate: f32) -> f32 {
    let samples = (time_s * sample_rate).max(1.0);
    (-((1.0 + ratio) / ratio).ln() / samples).exp()
}

impl Adsr {
    /// `curve` in `[0, 1]`: 0 is nearly linear, 1 is a snappy RC discharge.
    #[allow(clippy::too_many_arguments)]
    pub fn set_params(
        &mut self,
        attack_ms: f32,
        decay_ms: f32,
        sustain: f32,
        release_ms: f32,
        curve: f32,
        time_scale: f32,
        sample_rate: f32,
    ) {
        let ratio_a = 10.0_f32.powf(2.0 - 5.0 * curve);
        let ratio_dr = 10.0_f32.powf(1.0 - 5.0 * curve);

        // A sub-millisecond attack would be a step discontinuity.
        let a = (attack_ms * time_scale).max(0.5) * 1.0e-3;
        let d = (decay_ms * time_scale).max(1.0) * 1.0e-3;
        let r = (release_ms * time_scale).max(1.0) * 1.0e-3;

        self.sustain = sustain;
        self.a_coef = coef(a, ratio_a, sample_rate);
        self.a_base = (1.0 + ratio_a) * (1.0 - self.a_coef);
        self.d_coef = coef(d, ratio_dr, sample_rate);
        self.d_base = (sustain - ratio_dr) * (1.0 - self.d_coef);
        self.r_coef = coef(r, ratio_dr, sample_rate);
        self.r_base = -ratio_dr * (1.0 - self.r_coef);
    }

    /// Start (or restart) the envelope. A retrigger deliberately continues from
    /// the current level rather than snapping to zero.
    #[inline]
    pub fn trigger(&mut self) {
        self.stage = EnvStage::Attack;
    }

    #[inline]
    pub fn release(&mut self) {
        if self.stage != EnvStage::Idle && self.stage != EnvStage::Finished {
            self.stage = EnvStage::Release;
        }
    }

    #[inline]
    pub fn reset(&mut self) {
        self.out = 0.0;
        self.stage = EnvStage::Idle;
    }

    #[inline]
    pub fn is_finished(&self) -> bool {
        matches!(self.stage, EnvStage::Finished | EnvStage::Idle)
    }

    // `next` is the house style for every per-sample DSP block here; an
    // envelope is not an iterator and has no meaningful `None`.
    #[allow(clippy::should_implement_trait)]
    #[inline(always)]
    pub fn next(&mut self) -> f32 {
        match self.stage {
            EnvStage::Idle | EnvStage::Finished => self.out = 0.0,
            EnvStage::Attack => {
                self.out = self.a_base + self.out * self.a_coef;
                if self.out >= 1.0 {
                    self.out = 1.0;
                    self.stage = EnvStage::Decay;
                }
            }
            EnvStage::Decay => {
                self.out = self.d_base + self.out * self.d_coef;
                if self.out <= self.sustain {
                    self.out = self.sustain;
                    self.stage = EnvStage::Sustain;
                }
            }
            EnvStage::Sustain => self.out = self.sustain,
            EnvStage::Release => {
                self.out = self.r_base + self.out * self.r_coef;
                if self.out <= ENV_EPSILON {
                    self.out = 0.0;
                    self.stage = EnvStage::Finished;
                }
            }
        }
        self.out
    }
}

// ---------------------------------------------------------------------------
// LFO
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default)]
pub struct Lfo {
    pub phase: f32,
    pub value: f32,
    sh_value: f32,
    smooth: f32,
    smooth_target: f32,
    elapsed_s: f32,
    rng: Rng32,
}

impl Lfo {
    pub fn reset(&mut self, phase: f32, seed: u32) {
        self.phase = wrap01(phase);
        self.value = 0.0;
        self.sh_value = 0.0;
        self.smooth = 0.0;
        self.smooth_target = 0.0;
        self.elapsed_s = 0.0;
        self.rng = Rng32::new(seed);
    }

    /// Cycle length in seconds for a tempo-synced division.
    pub fn synced_hz(division: SyncDivision, tempo_bpm: f64) -> f32 {
        let beats = division.beats() as f64;
        ((tempo_bpm / 60.0) / beats) as f32
    }

    /// Advance by `dt` seconds and return the shaped value in `[-1, 1]`.
    #[inline]
    pub fn tick(&mut self, shape: LfoShape, rate_hz: f32, dt: f32) -> f32 {
        let step = rate_hz * dt;
        let prev = self.phase;
        self.phase = wrap01(self.phase + step);
        let wrapped = self.phase < prev;
        self.elapsed_s += dt;

        self.value = match shape {
            LfoShape::Sine => sin_lut(self.phase),
            LfoShape::Triangle => 4.0 * (self.phase - 0.5).abs() - 1.0,
            LfoShape::SawUp => 2.0 * self.phase - 1.0,
            LfoShape::SawDown => 1.0 - 2.0 * self.phase,
            LfoShape::Square => {
                if self.phase < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            LfoShape::SampleHold => {
                if wrapped {
                    self.sh_value = self.rng.bipolar();
                }
                self.sh_value
            }
            LfoShape::SmoothRandom => {
                if wrapped {
                    self.smooth_target = self.rng.bipolar();
                }
                // One-pole toward the target, time-constant tied to the rate so
                // the shape stays the same as the rate changes.
                let a = (step * 6.0).clamp(0.0, 1.0);
                self.smooth += a * (self.smooth_target - self.smooth);
                self.smooth
            }
        };
        self.value
    }

    /// Delay-then-fade-in envelope applied on top of the LFO depth.
    #[inline]
    pub fn envelope(&self, delay_ms: f32, fade_ms: f32) -> f32 {
        let t = self.elapsed_s * 1000.0;
        if t < delay_ms {
            0.0
        } else if fade_ms <= 0.0 {
            1.0
        } else {
            ((t - delay_ms) / fade_ms).min(1.0)
        }
    }
}
