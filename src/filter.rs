//! Zero-delay-feedback (TPT) transistor ladder with Oberheim-style tap mixing.

use crate::analog::tanh_fast;
use crate::params::FilterMode;

/// Resonance at which the ideal ladder oscillates is 4; a little past that
/// guarantees self-oscillation once the nonlinearity has taken its cut.
const K_MAX: f32 = 4.2;
/// Hard ceiling on the compensated feedback so the loop can never run away.
const K_CEILING: f32 = 5.0;

/// Mix weights over the five taps `[y0, y1, y2, y3, y4]`. Binomial expansions
/// of `(1 - LP)^n`, with the bandpasses scaled for unit peak gain.
#[inline]
pub fn mode_taps(mode: FilterMode) -> [f32; 5] {
    match mode {
        FilterMode::Lp24 => [0.0, 0.0, 0.0, 0.0, 1.0],
        FilterMode::Lp12 => [0.0, 0.0, 1.0, 0.0, 0.0],
        FilterMode::Hp24 => [1.0, -4.0, 6.0, -4.0, 1.0],
        FilterMode::Hp12 => [1.0, -2.0, 1.0, 0.0, 0.0],
        FilterMode::Bp24 => [0.0, 0.0, 4.0, -8.0, 4.0],
        FilterMode::Bp12 => [0.0, 2.0, -2.0, 0.0, 0.0],
        FilterMode::Notch => [1.0, -2.0, 2.0, 0.0, 0.0],
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Ladder {
    s: [f32; 4],
    /// Previous output, used only to estimate the local tanh slope.
    y_prev: f32,
    /// Smoothed slope, so drive compensation tracks level rather than waveform.
    a_smooth: f32,
    /// Per-sample-ramped instantaneous gain of one stage.
    pub g: f32,
    g_inc: f32,
    /// Per-stage gain mismatch from the voice's component tolerances.
    pub mismatch: [f32; 4],
}

impl Ladder {
    /// A ladder with no component mismatch, i.e. a mathematically ideal one.
    pub fn ideal() -> Self {
        Self {
            mismatch: [1.0; 4],
            ..Default::default()
        }
    }

    pub fn reset(&mut self) {
        self.s = [0.0; 4];
        self.y_prev = 0.0;
        self.a_smooth = 1.0;
        self.g_inc = 0.0;
    }

    /// Convert a cutoff in Hz to the TPT instantaneous gain `G = g / (1 + g)`.
    #[inline]
    pub fn gain_for(cutoff_hz: f32, sample_rate: f32) -> f32 {
        let fc = cutoff_hz.clamp(8.0, 0.45 * sample_rate);
        let g = (std::f32::consts::PI * fc / sample_rate).tan();
        g / (1.0 + g)
    }

    /// Set the target `G` for the coming control block; `next()` ramps to it.
    #[inline]
    pub fn set_target_gain(&mut self, target: f32, block_len: usize) {
        if self.g == 0.0 {
            self.g = target;
        }
        self.g_inc = (target - self.g) / block_len.max(1) as f32;
    }

    #[inline]
    pub fn snap_gain(&mut self, target: f32) {
        self.g = target;
        self.g_inc = 0.0;
    }

    /// One sample. `k` is the raw resonance feedback, `reso_comp` trades the
    /// authentic droop-under-drive against constant resonance.
    #[inline(always)]
    pub fn next(&mut self, input: f32, k: f32, reso_comp: f32, taps: &[f32; 5]) -> f32 {
        self.g += self.g_inc;
        let g = self.g.clamp(1.0e-5, 0.9999);

        let g1 = g * self.mismatch[0];
        let g2 = g * self.mismatch[1];
        let g3 = g * self.mismatch[2];
        let g4 = g * self.mismatch[3];

        let b1 = (1.0 - g1) * self.s[0];
        let b2 = (1.0 - g2) * self.s[1];
        let b3 = (1.0 - g3) * self.s[2];
        let b4 = (1.0 - g4) * self.s[3];

        // Contribution of the stored state to the fourth stage's output.
        let s_sum = g4 * g3 * g2 * b1 + g4 * g3 * b2 + g4 * b3 + b4;
        let g_prod = g1 * g2 * g3 * g4;

        // Secant linearisation of the feedback tanh: exact when linear, and
        // since a <= 1 it can only reduce loop gain, so it cannot destabilise.
        let a = if self.y_prev.abs() > 1.0e-6 {
            (tanh_fast(self.y_prev) / self.y_prev).clamp(0.05, 1.0)
        } else {
            1.0
        };
        self.a_smooth += 0.02 * (a - self.a_smooth);

        let k_eff = (k * (1.0 - reso_comp + reso_comp / self.a_smooth.max(0.15)))
            .clamp(0.0, K_CEILING);

        let y4 = (g_prod * input + s_sum) / (1.0 + k_eff * a * g_prod);
        let u = input - k_eff * a * y4;

        // Run the stages forward to update state. The last one reproduces the
        // solved y4 to within rounding.
        let y0 = u;
        let v1 = (y0 - self.s[0]) * g1;
        let y1 = v1 + self.s[0];
        self.s[0] = y1 + v1;

        let v2 = (y1 - self.s[1]) * g2;
        let y2 = v2 + self.s[1];
        self.s[1] = y2 + v2;

        let v3 = (y2 - self.s[2]) * g3;
        let y3 = v3 + self.s[2];
        self.s[2] = y3 + v3;

        let v4 = (y3 - self.s[3]) * g4;
        let y4_fwd = v4 + self.s[3];
        self.s[3] = y4_fwd + v4;

        self.y_prev = y4_fwd;

        taps[0] * y0 + taps[1] * y1 + taps[2] * y2 + taps[3] * y3 + taps[4] * y4_fwd
    }
}

/// Resonance parameter to ladder feedback.
#[inline]
pub fn resonance_to_k(resonance: f32) -> f32 {
    (resonance.clamp(0.0, 1.0) * K_MAX).min(K_MAX)
}

/// Magnitude response of the linear ladder at `freq`, for the GUI curve.
pub fn magnitude(mode: FilterMode, cutoff_hz: f32, resonance: f32, freq: f32, sample_rate: f32) -> f32 {
    use std::f32::consts::TAU;

    let g = Ladder::gain_for(cutoff_hz, sample_rate);
    let k = resonance_to_k(resonance);
    let w = TAU * freq / sample_rate;
    let (sin_w, cos_w) = w.sin_cos();

    // One TPT lowpass stage: H(z) = G(1 + z^-1) / (1 - (1 - 2G) z^-1)
    let num = (g * (1.0 + cos_w), g * -sin_w);
    let pole = 1.0 - 2.0 * g;
    let den = (1.0 - pole * cos_w, pole * sin_w);
    let lp = cdiv(num, den);

    let mut stage = [(1.0_f32, 0.0_f32); 5];
    for i in 1..5 {
        stage[i] = cmul(stage[i - 1], lp);
    }

    // Close the resonance loop: taps are divided by 1 + k * LP^4.
    let loop_den = cadd((1.0, 0.0), cscale(stage[4], k));
    let taps = mode_taps(mode);

    let mut acc = (0.0_f32, 0.0_f32);
    for i in 0..5 {
        acc = cadd(acc, cscale(stage[i], taps[i]));
    }
    let h = cdiv(acc, loop_den);
    (h.0 * h.0 + h.1 * h.1).sqrt()
}

#[inline]
fn cmul(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}
#[inline]
fn cadd(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    (a.0 + b.0, a.1 + b.1)
}
#[inline]
fn cscale(a: (f32, f32), s: f32) -> (f32, f32) {
    (a.0 * s, a.1 * s)
}
#[inline]
fn cdiv(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    let d = (b.0 * b.0 + b.1 * b.1).max(1.0e-20);
    ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
}
