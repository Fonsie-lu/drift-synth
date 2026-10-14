//! Global effects and the output stage.

use crate::analog::tanh_fast;
use crate::osc::sin_lut;
use crate::params::ChorusMode;

/// Power of two so the read index masks instead of branching.
const DELAY_LEN: usize = 8192;
const DELAY_MASK: usize = DELAY_LEN - 1;

/// Base tap delays in milliseconds. Deliberately non-harmonic so the three
/// combs never reinforce each other.
const TAPS_LUSH: [f32; 3] = [8.0, 12.5, 17.0];
const TAPS_VINTAGE: [f32; 3] = [2.5, 4.0, 5.5];

/// Mutually irrational rate multipliers, so the taps never re-phase.
const RATE_MULT: [f32; 3] = [1.000, 1.414, 1.933];
const PHASE_OFFSET: [f32; 3] = [0.0, 1.0 / 3.0, 2.0 / 3.0];
/// Right channel runs in quadrature: as wide as the Juno's 180 degrees but it
/// still sums to mono without mid-band cancellation.
const QUADRATURE: f32 = 0.25;

const WET_NORM: f32 = 0.577_350_3; // 1 / sqrt(3)
/// Peak pitch deviation cap, about 26 cents.
const MAX_PITCH_DEV: f32 = 0.015;

/// Catmull-Rom. Stateless, unlike an allpass, which chirps when the delay
/// length changes continuously - and here it changes continuously by design.
#[inline(always)]
fn hermite(ym1: f32, y0: f32, y1: f32, y2: f32, t: f32) -> f32 {
    let c1 = 0.5 * (y1 - ym1);
    let c2 = ym1 - 2.5 * y0 + 2.0 * y1 - 0.5 * y2;
    let c3 = 0.5 * (y2 - ym1) + 1.5 * (y0 - y1);
    ((c3 * t + c2) * t + c1) * t + y0
}

pub struct Chorus {
    left: Box<[f32; DELAY_LEN]>,
    right: Box<[f32; DELAY_LEN]>,
    write: usize,
    phase: [f32; 3],
    fb_l: f32,
    fb_r: f32,
    lp_l: f32,
    lp_r: f32,
    sample_rate: f32,
    lp_coeff: f32,
}

impl Default for Chorus {
    fn default() -> Self {
        Self {
            left: Box::new([0.0; DELAY_LEN]),
            right: Box::new([0.0; DELAY_LEN]),
            write: 0,
            phase: PHASE_OFFSET,
            fb_l: 0.0,
            fb_r: 0.0,
            lp_l: 0.0,
            lp_r: 0.0,
            sample_rate: 48_000.0,
            lp_coeff: 0.5,
        }
    }
}

impl Chorus {
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        // 6 kHz one-pole in the feedback path so repeats darken.
        self.lp_coeff = 1.0 - (-std::f32::consts::TAU * 6000.0 / sample_rate).exp();
    }

    pub fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write = 0;
        self.phase = PHASE_OFFSET;
        self.fb_l = 0.0;
        self.fb_r = 0.0;
        self.lp_l = 0.0;
        self.lp_r = 0.0;
    }

    #[inline(always)]
    fn read(buf: &[f32; DELAY_LEN], write: usize, delay: f32) -> f32 {
        let delay = delay.clamp(2.0, (DELAY_LEN - 4) as f32);
        let int = delay.floor();
        let frac = delay - int;
        let base = write + DELAY_LEN - int as usize;
        let ym1 = buf[(base + 1) & DELAY_MASK];
        let y0 = buf[base & DELAY_MASK];
        let y1 = buf[(base - 1) & DELAY_MASK];
        let y2 = buf[(base - 2) & DELAY_MASK];
        hermite(ym1, y0, y1, y2, frac)
    }

    /// One stereo sample. `rate` in Hz, `depth`/`feedback`/`mix` in `[0, 1]`.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn next(
        &mut self,
        in_l: f32,
        in_r: f32,
        mode: ChorusMode,
        rate: f32,
        depth: f32,
        feedback: f32,
        mix: f32,
    ) -> (f32, f32) {
        if mix <= 1.0e-5 {
            // Keep the lines primed so enabling the effect does not click.
            self.left[self.write] = in_l;
            self.right[self.write] = in_r;
            self.write = (self.write + 1) & DELAY_MASK;
            return (in_l, in_r);
        }

        let taps = match mode {
            ChorusMode::Lush => TAPS_LUSH,
            ChorusMode::Vintage => TAPS_VINTAGE,
        };

        self.left[self.write] = in_l + self.fb_l * feedback;
        self.right[self.write] = in_r + self.fb_r * feedback;

        let inv_sr = 1.0 / self.sample_rate;
        let mut wet_l = 0.0;
        let mut wet_r = 0.0;
        let mut tap1_l = 0.0;
        let mut tap1_r = 0.0;

        for i in 0..3 {
            let f = rate * RATE_MULT[i];
            self.phase[i] += f * inv_sr;
            if self.phase[i] >= 1.0 {
                self.phase[i] -= 1.0;
            }

            // Peak pitch deviation of a sinusoidally modulated delay is
            // 2*pi*f*delta, so cap delta as the rate rises. Without this the
            // top of the RATE range is unusable at any real depth.
            let delta_s = (depth * 0.003).min(MAX_PITCH_DEV / (std::f32::consts::TAU * f.max(0.01)));
            let delta_samples = delta_s * self.sample_rate;
            let base_samples = taps[i] * 1.0e-3 * self.sample_rate;

            let mod_l = sin_lut(self.phase[i]);
            let mod_r = sin_lut(self.phase[i] + QUADRATURE);

            let l = Self::read(&self.left, self.write, base_samples + delta_samples * mod_l);
            let r = Self::read(&self.right, self.write, base_samples + delta_samples * mod_r);
            wet_l += l;
            wet_r += r;
            if i == 0 {
                tap1_l = l;
                tap1_r = r;
            }
        }

        // Feedback comes off tap 1 only; taking all three turns it into a mess.
        self.lp_l += self.lp_coeff * (tap1_l - self.lp_l);
        self.lp_r += self.lp_coeff * (tap1_r - self.lp_r);
        self.fb_l = self.lp_l;
        self.fb_r = self.lp_r;

        self.write = (self.write + 1) & DELAY_MASK;

        wet_l *= WET_NORM;
        wet_r *= WET_NORM;
        (in_l + (wet_l - in_l) * mix, in_r + (wet_r - in_r) * mix)
    }
}

/// 5 Hz one-pole highpass. Mandatory, because the asymmetric VCA saturation
/// generates DC by construction.
#[derive(Clone, Copy, Debug, Default)]
pub struct DcBlocker {
    x1: f32,
    y1: f32,
    r: f32,
}

impl DcBlocker {
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.r = (-std::f32::consts::TAU * 5.0 / sample_rate).exp();
    }
    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }
    #[inline(always)]
    pub fn next(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + self.r * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}

/// One-pole lowpass used for the analog output tilt.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnePoleLp {
    z: f32,
    a: f32,
}

impl OnePoleLp {
    pub fn set_cutoff(&mut self, hz: f32, sample_rate: f32) {
        self.a = (1.0 - (-std::f32::consts::TAU * hz / sample_rate).exp()).clamp(0.0, 1.0);
    }
    pub fn reset(&mut self) {
        self.z = 0.0;
    }
    #[inline(always)]
    pub fn next(&mut self, x: f32) -> f32 {
        self.z += self.a * (x - self.z);
        self.z
    }
}

/// Soft limiter. `tanh` has unity slope at zero so quiet material is untouched;
/// the clamp catches the residue. This is sample-domain only, so inter-sample
/// peaks can still exceed 0 dBFS after reconstruction.
#[inline(always)]
pub fn limit(x: f32) -> f32 {
    tanh_fast(x * 0.944).clamp(-1.0, 1.0)
}
