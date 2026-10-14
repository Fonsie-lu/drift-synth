//! Analog character: per-voice component tolerances, slow drift, phase jitter
//! and pink noise. Everything here is scaled by the single `ANALOG` macro, and
//! at `analog == 0` the whole block is bypassed so the synth is exactly clean.

/// xorshift32. Cheap, good enough for noise and tolerances, and deterministic
/// per voice so a rendered note is reproducible.
#[derive(Clone, Copy, Debug)]
pub struct Rng32 {
    state: u32,
}

impl Default for Rng32 {
    fn default() -> Self {
        Self::new(0x2545_F491)
    }
}

impl Rng32 {
    pub const fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0x2545_F491 } else { seed },
        }
    }

    #[inline(always)]
    pub fn next_u32(&mut self) -> u32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 17;
        self.state ^= self.state << 5;
        self.state
    }

    /// Uniform in `[-1, 1)`.
    #[inline(always)]
    pub fn bipolar(&mut self) -> f32 {
        ((self.next_u32() >> 8) as f32) * (1.0 / 8_388_608.0) - 1.0
    }

    /// Uniform in `[0, 1)`.
    #[inline(always)]
    pub fn unipolar(&mut self) -> f32 {
        ((self.next_u32() >> 8) as f32) * (1.0 / 16_777_216.0)
    }
}

// ---------------------------------------------------------------------------
// Slow drift
// ---------------------------------------------------------------------------

/// Corner frequencies of the three drift stages, log-spaced by roughly 7x.
const DRIFT_HZ: [f32; 3] = [0.10, 0.70, 4.70];
/// 1/f-ish weighting across the three stages.
const DRIFT_W: [f32; 3] = [1.00, 0.50, 0.25];
const DRIFT_SUM_RECIP: f32 = 1.0 / (1.00 + 0.50 + 0.25);
/// Uniform noise in [-1, 1) has variance 1/3, so this scales it to unit variance.
const SQRT_3: f32 = 1.732_050_8;

/// Per-rate coefficients, computed once in `initialize()`.
#[derive(Clone, Copy, Debug, Default)]
pub struct DriftCoeffs {
    a: [f32; 3],
    norm: [f32; 3],
}

impl DriftCoeffs {
    /// `rate_hz` is the rate the generator is ticked at, i.e. the control rate.
    pub fn new(rate_hz: f32) -> Self {
        let mut a = [0.0; 3];
        let mut norm = [0.0; 3];
        for i in 0..3 {
            let coef = 1.0 - (-std::f32::consts::TAU * DRIFT_HZ[i] / rate_hz).exp();
            a[i] = coef.clamp(1.0e-6, 1.0);
            // A one-pole on unit-variance white noise has variance a/(2-a).
            norm[i] = ((2.0 - a[i]) / a[i]).sqrt();
        }
        Self { a, norm }
    }
}

/// Three parallel one-poles on white noise. Chosen over summed LFOs, which
/// repeat audibly within about half a minute, and over raw pink noise, which
/// has far too much energy above the band we want.
#[derive(Clone, Copy, Debug, Default)]
pub struct Drift {
    z: [f32; 3],
}

impl Drift {
    #[inline]
    pub fn reset(&mut self) {
        self.z = [0.0; 3];
    }

    /// Roughly unit RMS, hard-limited so a tail event cannot throw the pitch.
    #[inline]
    pub fn tick(&mut self, rng: &mut Rng32, c: &DriftCoeffs) -> f32 {
        // Uniform [-1,1) has variance 1/3; scale to unit variance.
        let white = rng.bipolar() * SQRT_3;
        let mut acc = 0.0;
        for (i, z) in self.z.iter_mut().enumerate() {
            *z += c.a[i] * (white - *z);
            acc += DRIFT_W[i] * c.norm[i] * *z;
        }
        (acc * DRIFT_SUM_RECIP).clamp(-3.0, 3.0)
    }
}

// ---------------------------------------------------------------------------
// Pink noise
// ---------------------------------------------------------------------------

/// Kellet's economy three-pole pink filter. The coefficients are tuned for
/// 44.1 kHz; the tilt drifts a couple of dB at 96 kHz, which does not matter
/// for a noise floor sitting 54 dB down.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pink {
    b: [f32; 3],
}

impl Pink {
    #[inline]
    pub fn reset(&mut self) {
        self.b = [0.0; 3];
    }

    #[inline(always)]
    pub fn next(&mut self, rng: &mut Rng32) -> f32 {
        let w = rng.bipolar();
        self.b[0] = 0.997_65 * self.b[0] + w * 0.099_046_0;
        self.b[1] = 0.963_00 * self.b[1] + w * 0.296_516_4;
        self.b[2] = 0.570_00 * self.b[2] + w * 1.052_691_3;
        (self.b[0] + self.b[1] + self.b[2] + w * 0.184_8) * 0.12
    }
}

// ---------------------------------------------------------------------------
// Component tolerances
// ---------------------------------------------------------------------------

/// Drawn once when a voice is allocated and frozen for the life of the note.
/// Static offsets are what separate "analog" from "chorus" - a real VCO's
/// mismatch does not wobble.
#[derive(Clone, Copy, Debug)]
pub struct Tolerance {
    pub pitch_cents: [f32; 2],
    pub cutoff_st: f32,
    pub resonance: f32,
    pub env_scale: [f32; 3],
    pub gain: f32,
    pub pan_offset: f32,
    pub pulse_width: [f32; 2],
    pub stage_mismatch: [f32; 4],
}

impl Default for Tolerance {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

impl Tolerance {
    pub const NEUTRAL: Self = Self {
        pitch_cents: [0.0; 2],
        cutoff_st: 0.0,
        resonance: 0.0,
        env_scale: [1.0; 3],
        gain: 1.0,
        pan_offset: 0.0,
        pulse_width: [0.0; 2],
        stage_mismatch: [1.0; 4],
    };

    pub fn draw(rng: &mut Rng32, analog: f32) -> Self {
        if analog <= 1.0e-4 {
            return Self::NEUTRAL;
        }
        let a = analog;
        Self {
            pitch_cents: [rng.bipolar() * 5.0 * a, rng.bipolar() * 5.0 * a],
            cutoff_st: rng.bipolar() * 1.2 * a,
            resonance: rng.bipolar() * 0.04 * a,
            env_scale: [
                1.0 + rng.bipolar() * 0.08 * a,
                1.0 + rng.bipolar() * 0.08 * a,
                1.0 + rng.bipolar() * 0.08 * a,
            ],
            gain: 10.0_f32.powf(rng.bipolar() * 1.5 * a / 20.0),
            pan_offset: rng.bipolar() * 0.06 * a,
            pulse_width: [rng.bipolar() * 0.015 * a, rng.bipolar() * 0.015 * a],
            // Four slightly mismatched poles is literally what a real ladder
            // is, and it matters more than its size suggests.
            stage_mismatch: [
                1.0 + rng.bipolar() * 0.02 * a,
                1.0 + rng.bipolar() * 0.02 * a,
                1.0 + rng.bipolar() * 0.02 * a,
                1.0 + rng.bipolar() * 0.02 * a,
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// Depths, all at analog == 1
// ---------------------------------------------------------------------------

pub const DRIFT_PITCH_CENTS: f32 = 6.0;
pub const DRIFT_CUTOFF_CENTS: f32 = 40.0;
pub const DRIFT_AMP: f32 = 0.01;
pub const GLOBAL_DRIFT_CENTS: f32 = 3.0;
/// Phase jitter depth in cycles; 2.5e-4 puts the noise floor near -56 dBc.
pub const JITTER_SIGMA: f32 = 2.5e-4;
/// Pink noise into the filter input, relative to full scale.
pub const NOISE_LEVEL: f32 = 0.002;
/// Always-on seed so the ladder can start self-oscillating and to keep the
/// filter states out of denormal range.
pub const NOISE_FLOOR: f32 = 1.0e-5;
/// Coefficient of the asymmetric second-harmonic term at the VCA.
pub const VCA_ASYM: f32 = 0.04;

/// Rational tanh. Deliberately inexact - it is a saturator.
#[inline(always)]
pub fn tanh_fast(x: f32) -> f32 {
    let x = x.clamp(-5.0, 5.0);
    let x2 = x * x;
    x * (27.0 + x2) / (27.0 + 9.0 * x2)
}

/// Flush-to-zero / denormals-are-zero for the duration of one `process()`
/// call, restored on drop. MXCSR is per-thread and hosts may hand us a
/// different thread or reset it between plugins, so this has to be re-armed
/// every callback rather than once at init.
pub struct DenormalGuard {
    #[cfg(target_arch = "x86_64")]
    saved: u32,
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn read_mxcsr() -> u32 {
    let mut value: u32 = 0;
    // SAFETY: writes exactly four bytes to a live local.
    unsafe {
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut value, options(nostack, preserves_flags));
    }
    value
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn write_mxcsr(value: u32) {
    // SAFETY: reads exactly four bytes from a live local.
    unsafe {
        std::arch::asm!("ldmxcsr [{}]", in(reg) &value, options(nostack, preserves_flags));
    }
}

impl DenormalGuard {
    #[inline]
    pub fn new() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            let saved = read_mxcsr();
            // bit 15 = flush-to-zero, bit 6 = denormals-are-zero
            write_mxcsr(saved | 0x8000 | 0x0040);
            Self { saved }
        }
        #[cfg(not(target_arch = "x86_64"))]
        Self {}
    }
}

impl Default for DenormalGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DenormalGuard {
    #[inline]
    fn drop(&mut self) {
        #[cfg(target_arch = "x86_64")]
        write_mxcsr(self.saved);
    }
}
