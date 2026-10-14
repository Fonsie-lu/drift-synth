//! The FM modulator operator.
//!
//! This is phase modulation, not frequency modulation: the modulator offsets
//! the read phase instead of being integrated into the accumulator. That means
//! DC in the modulator cannot make the pitch run away, automating the index
//! does not bend the pitch, and - because the index is set directly rather
//! than derived as `df / f_mod` - the timbre stays the same at every pitch in
//! ratio mode. True FM with a constant deviation gets duller as you play up.

use crate::osc::{sin_lut, wrap01};
use crate::params::FmRatioMode;

/// Ratios the harmonic mode snaps to.
pub const HARMONIC_RATIOS: [f32; 13] = [
    0.5, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 11.0, 13.0, 16.0,
];

/// Feedback beyond about 1 rad stops having a unique solution and turns noisy;
/// 1.6 rad is musically useful without becoming pure hash.
const MAX_FEEDBACK_RAD: f32 = 1.6;

#[derive(Clone, Copy, Debug, Default)]
pub struct ModOsc {
    pub phase: f32,
    pub inc: f32,
    y1: f32,
    y2: f32,
}

impl ModOsc {
    #[inline]
    pub fn reset(&mut self, phase: f32) {
        self.phase = wrap01(phase);
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    /// One sample. `feedback` is in cycles.
    ///
    /// The two-sample average is what keeps this stable: it is a FIR with a
    /// zero exactly at Nyquist, which kills the alternating-sign limit cycle
    /// that naive single-sample feedback sustains as a fixed whine.
    #[inline(always)]
    pub fn next(&mut self, feedback: f32) -> f32 {
        let averaged = 0.5 * (self.y1 + self.y2);
        let y = sin_lut(self.phase + feedback * averaged);
        self.y2 = self.y1;
        self.y1 = y;
        self.phase = wrap01(self.phase + self.inc);
        y
    }
}

/// Map the feedback parameter to a phase offset in cycles.
#[inline]
pub fn feedback_cycles(param: f32) -> f32 {
    param.clamp(0.0, 1.0) * MAX_FEEDBACK_RAD / std::f32::consts::TAU
}

/// Resolve the modulator frequency for one voice.
pub fn modulator_hz(
    mode: FmRatioMode,
    ratio: f32,
    ratio_fine_cents: f32,
    fixed_hz: f32,
    carrier_hz: f32,
) -> f32 {
    match mode {
        FmRatioMode::Fixed => fixed_hz,
        FmRatioMode::Harmonic => {
            let snapped = HARMONIC_RATIOS
                .iter()
                .copied()
                .min_by(|a, b| {
                    (a - ratio)
                        .abs()
                        .partial_cmp(&(b - ratio).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap_or(1.0);
            carrier_hz * snapped * cents_ratio(ratio_fine_cents)
        }
        FmRatioMode::Free => carrier_hz * ratio * cents_ratio(ratio_fine_cents),
    }
}

#[inline]
pub fn cents_ratio(cents: f32) -> f32 {
    (cents / 1200.0).exp2()
}

/// Largest index, in cycles, whose Carson-rule bandwidth still fits below
/// 0.45 * sample rate. Sidebands reach `f_c + (beta + 1) * f_mod`.
#[inline]
pub fn max_index_cycles(modulator_hz: f32, sample_rate: f32) -> f32 {
    let beta_rad = (0.45 * sample_rate / modulator_hz.max(1.0) - 1.0).max(0.0);
    beta_rad / std::f32::consts::TAU
}

/// Bipolar envelope shaping of the index. Positive fades the index in with the
/// envelope, negative inverts it so the index swells instead of decaying.
#[inline]
pub fn env_multiplier(env_amount: f32, env: f32) -> f32 {
    if env_amount >= 0.0 {
        (1.0 - env_amount) + env_amount * env
    } else {
        (1.0 + env_amount) + (-env_amount) * (1.0 - env)
    }
}

/// Optional index keytrack. The exponent is negative so the index shrinks as
/// you play up, which both mimics acoustic instruments and reduces aliasing.
#[inline]
pub fn keytrack_scale(keytrack: f32, carrier_hz: f32) -> f32 {
    if keytrack <= 0.0 {
        1.0
    } else {
        (carrier_hz / 261.625_57).powf(-keytrack)
    }
}
