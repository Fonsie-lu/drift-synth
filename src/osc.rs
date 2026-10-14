//! Oscillators: band-limited classic waveforms and the additive partial bank.

use crate::params::Waveform;
use crate::N_PARTIALS;

include!(concat!(env!("OUT_DIR"), "/sine_lut.rs"));

/// Geometric centre of the partial range, `sqrt(1 * N_PARTIALS)`. Tilt pivots
/// here so it changes spectral slope without changing overall level much.
const TILT_PIVOT: f32 = std::f32::consts::SQRT_2 * 2.0;

/// Partials start fading out at this fraction of Nyquist and are gone at 1.0.
const NYQ_FADE_START: f32 = 0.90;

/// Peak-sum normalisation leaves spread-out spectra quiet; this wins some back.
const ADDITIVE_MAKEUP: f32 = 1.4;

#[inline(always)]
pub fn wrap01(x: f32) -> f32 {
    x - x.floor()
}

/// Interpolated sine of a phase given in cycles. Accepts any real input.
#[inline(always)]
pub fn sin_lut(phase: f32) -> f32 {
    let x = wrap01(phase) * SINE_LUT_SIZE as f32;
    let i = (x as usize) & (SINE_LUT_SIZE - 1);
    let frac = x - x.floor();
    let a = SINE_LUT[i];
    a + frac * (SINE_LUT[i + 1] - a)
}

/// Two-point PolyBLEP residual. Jumps by -2 at `t == 0`, zero outside the
/// kernel. Add `h / 2 * poly_blep(..)` for a naive step of height `h`.
#[inline(always)]
pub fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        2.0 * x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}

/// Integral of `poly_blep`: continuous, its derivative jumps by -2 at `t == 0`.
/// Add `delta_slope / 2 * poly_blamp(..)` at a corner.
#[inline(always)]
pub fn poly_blamp(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt - 1.0;
        -(x * x * x) / 3.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt + 1.0;
        (x * x * x) / 3.0
    } else {
        0.0
    }
}

#[inline(always)]
fn saw_blep(t: f32, dt: f32) -> f32 {
    (2.0 * t - 1.0) - poly_blep(t, dt)
}

/// Pulse as the difference of two band-limited saws, so both edges are
/// corrected by the same code path.
#[inline(always)]
fn pulse(t: f32, dt: f32, width: f32) -> f32 {
    saw_blep(t, dt) - saw_blep(wrap01(t - width), dt) - (2.0 * width - 1.0)
}

/// Triangle via PolyBLAMP. Deliberately stateless rather than an integrated
/// square: phase randomisation and phase modulation both make the phase jump
/// or run backwards, which an integrator cannot survive.
#[inline(always)]
fn triangle(t: f32, dt: f32) -> f32 {
    let mut y = 4.0 * (t - 0.5).abs() - 1.0;
    y -= 4.0 * dt * poly_blamp(t, dt);
    y += 4.0 * dt * poly_blamp(wrap01(t + 0.5), dt);
    y
}

#[inline(always)]
pub fn subtractive(wave: Waveform, t: f32, dt: f32, width: f32) -> f32 {
    match wave {
        Waveform::Sine => sin_lut(t),
        Waveform::Triangle => triangle(t, dt),
        Waveform::Saw => saw_blep(t, dt),
        Waveform::Pulse => pulse(t, dt, width),
    }
}

/// Sum of `N_PARTIALS` harmonics off the shared LUT. The phase offset is
/// multiplied by the harmonic number, which is what makes phase modulation act
/// on the composite waveform rather than on each partial separately.
#[inline(always)]
pub fn additive(phase: f32, gains: &[f32; N_PARTIALS]) -> f32 {
    let mut y = 0.0;
    for (k, g) in gains.iter().enumerate() {
        y += g * sin_lut((k as f32 + 1.0) * phase);
    }
    y
}

/// Resolve the eight drawbars plus the three macros into per-partial gains.
///
/// The macros multiply rather than offset, so a drawbar pulled to zero stays
/// at zero. `tilt` is linear in dB against log frequency and `decay_db` is
/// linear in dB against partial index, which keeps the two genuinely
/// independent instead of collapsing into the same power law.
pub fn partial_gains(
    sliders: &[f32; N_PARTIALS],
    tilt: f32,
    decay_db: f32,
    odd_even: f32,
    f0: f32,
    nyquist: f32,
) -> [f32; N_PARTIALS] {
    let decay = 10.0_f32.powf(decay_db / 20.0).max(1.0e-3);
    let odd_cut = 1.0 - odd_even.max(0.0);
    let even_cut = 1.0 - (-odd_even).max(0.0);

    let mut out = [0.0_f32; N_PARTIALS];
    let mut sum = 0.0_f32;

    for (idx, slot) in out.iter_mut().enumerate() {
        let k = idx as f32 + 1.0;

        let a_dec = decay.powf(idx as f32 / (N_PARTIALS as f32 - 1.0));
        let a_tilt = (k / TILT_PIVOT).powf(2.0 * tilt);
        let a_oe = if idx == 0 {
            // The fundamental always survives; killing it drops the perceived
            // pitch an octave, which is a bad default for an "even" setting.
            1.0
        } else if idx % 2 == 0 {
            odd_cut
        } else {
            even_cut
        };

        let value = sliders[idx] * a_dec * a_tilt * a_oe * nyquist_fade(k * f0, nyquist);
        *slot = value;
        sum += value.abs();
    }

    // All partials share a phasor, so they align at phase 0 and the true peak
    // is the sum of magnitudes.
    let norm = ADDITIVE_MAKEUP / sum.max(1.0e-6);
    for slot in out.iter_mut() {
        *slot *= norm;
    }
    out
}

/// Raised-cosine fade so a partial that wobbles across Nyquist under pitch
/// drift fades instead of zippering.
#[inline]
pub fn nyquist_fade(freq: f32, nyquist: f32) -> f32 {
    let r = freq / nyquist;
    if r <= NYQ_FADE_START {
        1.0
    } else if r >= 1.0 {
        0.0
    } else {
        let x = (r - NYQ_FADE_START) / (1.0 - NYQ_FADE_START);
        0.5 * (1.0 + (std::f32::consts::PI * x).cos())
    }
}

/// One oscillator's running phase.
#[derive(Clone, Copy, Debug, Default)]
pub struct Osc {
    pub phase: f32,
    pub inc: f32,
}

impl Osc {
    #[inline(always)]
    pub fn reset(&mut self, phase: f32) {
        self.phase = wrap01(phase);
    }

    /// Render one sample and advance. `pm` is a phase offset in cycles,
    /// carrying both FM and the analog phase jitter.
    #[inline(always)]
    pub fn next(
        &mut self,
        wave: Waveform,
        width: f32,
        pm: f32,
        blend: f32,
        gains: &[f32; N_PARTIALS],
    ) -> f32 {
        let dt = self.inc.abs().clamp(1.0e-7, 0.45);
        let effective = self.phase + pm;

        let sub = if blend < 1.0 {
            subtractive(wave, wrap01(effective), dt, width)
        } else {
            0.0
        };
        let add = if blend > 0.0 {
            additive(effective, gains)
        } else {
            0.0
        };

        self.phase = wrap01(self.phase + self.inc);

        // Linear, not equal-power: both branches share a phasor and are
        // therefore correlated, so equal-power would bump +3 dB at the middle.
        sub + (add - sub) * blend
    }
}
