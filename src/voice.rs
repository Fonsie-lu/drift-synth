//! Voice allocation, unison grouping, stealing, and the per-voice render chain.

use crate::analog::{
    self, tanh_fast, Drift, DriftCoeffs, Pink, Rng32, Tolerance, DRIFT_AMP, DRIFT_CUTOFF_CENTS,
    DRIFT_PITCH_CENTS, JITTER_SIGMA, NOISE_FLOOR, VCA_ASYM,
};
use crate::env::{Adsr, EnvStage, Lfo};
use crate::filter::{self, Ladder};
use crate::fm::{self, ModOsc};
use crate::osc::{self, Osc};
use crate::params::{FilterMode, FmRatioMode, LfoDest, LfoShape, Waveform};
use crate::{DECLICK_VOICES, MAX_UNISON, N_PARTIALS, VOICE_SLOTS};

/// Length of the fade applied to a stolen voice. Long enough that the splatter
/// sits ~40 dB below an instantaneous cut, short enough to be imperceptible.
pub const STEAL_FADE_S: f32 = 0.002;

/// Below this peak amplitude a sounding group counts as "quiet" for stealing.
const QUIET_THRESHOLD: f32 = 0.02;

/// Headroom in front of the filter's input saturator.
///
/// Without it a nominal unity-amplitude oscillator already sits on the knee of
/// the tanh, so DRIVE at 0 dB is audibly dirty. Worse, a memoryless
/// nonlinearity generates harmonics above Nyquist that fold straight back into
/// the band, so an over-eager input stage costs real aliasing on top of the
/// oscillator's own. Dividing in and multiplying back out keeps the
/// small-signal gain at exactly 1 while leaving the knob its full range.
const DRIVE_HEADROOM: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceState {
    Idle,
    Active,
    /// Fading out after being stolen; its slot is not reusable yet.
    Stealing,
}

/// A voice that has ended and whose ID must be reported back to the host.
#[derive(Clone, Copy, Debug)]
pub struct Terminated {
    pub voice_id: i32,
    pub channel: u8,
    pub note: u8,
}

/// Everything the voice needs from the parameters for one control block.
/// Resolved once per block in `process()` and shared by every voice.
pub struct VoiceBlock {
    pub sample_rate: f32,
    pub nyquist: f32,
    pub inv_sample_rate: f32,

    pub wave: [Waveform; 2],
    pub coarse_cents: [f32; 2],
    pub pulse_width: [f32; 2],
    pub level: [f32; 2],
    pub blend: [f32; 2],
    pub sliders: [[f32; N_PARTIALS]; 2],
    pub tilt: [f32; 2],
    pub partial_decay_db: [f32; 2],
    pub odd_even: [f32; 2],

    pub fm_mode: FmRatioMode,
    pub fm_ratio: f32,
    pub fm_ratio_fine: f32,
    pub fm_fixed_hz: f32,
    pub fm_index: [f32; 2],
    pub fm_feedback: f32,
    pub fm_env_amount: f32,
    pub fm_keytrack: f32,

    pub filter_mode: FilterMode,
    pub filter_taps: [f32; 5],
    pub cutoff_hz: f32,
    pub resonance: f32,
    pub drive_gain: f32,
    pub filter_keytrack: f32,
    pub filter_env_amount: f32,
    pub filter_vel_amount: f32,
    pub reso_comp: f32,

    pub amp_adsr: [f32; 5],
    pub filter_adsr: [f32; 5],
    pub fm_adsr: [f32; 5],

    pub unison: usize,
    pub detune_cents: f32,
    pub spread: f32,
    pub analog: f32,
    pub noise_level: f32,

    /// Master tune plus pitch bend plus global drift, in cents.
    pub global_cents: f32,
    /// Per-voice LFO settings.
    pub lfo1_shape: LfoShape,
    pub lfo1_rate: f32,
    pub lfo1_depth: f32,
    pub lfo1_dest: LfoDest,
    pub lfo1_delay: f32,
    pub lfo1_fade: f32,
    pub vibrato_cents: f32,
    pub mod_wheel: f32,

    /// LFO 2 is global, so its value is resolved outside the voice.
    pub lfo2_value: f32,
    pub lfo2_dest: LfoDest,
    pub lfo2_depth: f32,

    pub drift_coeffs: DriftCoeffs,
    pub jitter_coeff: f32,
}

impl VoiceBlock {
    /// A neutral block: one saw oscillator, filter wide open, no FM, no
    /// analog character. Used by the offline renderer and the tests, and as a
    /// starting point when only a few fields need overriding.
    pub fn defaults(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            nyquist: sample_rate * 0.5,
            inv_sample_rate: 1.0 / sample_rate,

            wave: [Waveform::Saw; 2],
            coarse_cents: [0.0, 0.0],
            pulse_width: [0.5; 2],
            level: [1.0, 0.0],
            blend: [0.0; 2],
            sliders: [[1.0; N_PARTIALS]; 2],
            tilt: [0.0; 2],
            partial_decay_db: [-12.0; 2],
            odd_even: [0.0; 2],

            fm_mode: FmRatioMode::Harmonic,
            fm_ratio: 2.0,
            fm_ratio_fine: 0.0,
            fm_fixed_hz: 440.0,
            fm_index: [0.0; 2],
            fm_feedback: 0.0,
            fm_env_amount: 0.0,
            fm_keytrack: 0.0,

            filter_mode: FilterMode::Lp24,
            filter_taps: filter::mode_taps(FilterMode::Lp24),
            cutoff_hz: 20_000.0,
            resonance: 0.0,
            drive_gain: 1.0,
            filter_keytrack: 0.0,
            filter_env_amount: 0.0,
            filter_vel_amount: 0.0,
            reso_comp: 0.6,

            amp_adsr: [1.0, 1000.0, 1.0, 50.0, 0.5],
            filter_adsr: [1.0, 1000.0, 1.0, 50.0, 0.5],
            fm_adsr: [1.0, 1000.0, 1.0, 50.0, 0.5],

            unison: 1,
            detune_cents: 0.0,
            spread: 0.0,
            analog: 0.0,
            noise_level: 0.0,

            global_cents: 0.0,
            lfo1_shape: LfoShape::Triangle,
            lfo1_rate: 5.0,
            lfo1_depth: 0.0,
            lfo1_dest: LfoDest::Off,
            lfo1_delay: 0.0,
            lfo1_fade: 0.0,
            vibrato_cents: 0.0,
            mod_wheel: 0.0,

            lfo2_value: 0.0,
            lfo2_dest: LfoDest::Off,
            lfo2_depth: 0.0,

            drift_coeffs: DriftCoeffs::new(sample_rate / crate::CTL_BLOCK as f32),
            jitter_coeff: 1.0 - (-std::f32::consts::TAU * 1500.0 / sample_rate).exp(),
        }
    }

    /// Chorus mode is not a voice parameter, but the offline renderer needs a
    /// sensible default alongside the rest of the block.
    pub fn chorus_mode(&self) -> crate::params::ChorusMode {
        crate::params::ChorusMode::Lush
    }

    /// Signed modulation contribution of both LFOs for one destination.
    #[inline]
    fn lfo_amount(&self, dest: LfoDest, lfo1: f32, lfo1_env: f32) -> f32 {
        let mut acc = 0.0;
        if self.lfo1_dest == dest {
            acc += lfo1 * self.lfo1_depth * lfo1_env;
        }
        if self.lfo2_dest == dest {
            acc += self.lfo2_value * self.lfo2_depth;
        }
        acc
    }
}

pub struct Voice {
    pub state: VoiceState,
    pub note_id: u32,
    pub voice_id: i32,
    pub channel: u8,
    pub note: u8,
    pub velocity: f32,
    pub held_by_pedal: bool,
    pub releasing: bool,

    pub unison_x: f32,
    pub detune_jitter: f32,
    pub internal_id: u64,
    pub birth_block: u64,
    pub release_stamp: u64,

    pub steal_gain: f32,
    pub steal_dec: f32,

    pub osc: [Osc; 2],
    pub mod_osc: ModOsc,
    pub ladder: Ladder,
    pub amp_env: Adsr,
    pub filter_env: Adsr,
    pub fm_env: Adsr,
    pub lfo1: Lfo,

    pub rng: Rng32,
    pub drift_pitch: Drift,
    pub drift_cutoff: Drift,
    pub drift_amp: Drift,
    pub pink: Pink,
    pub jitter: [f32; 2],
    pub tol: Tolerance,

    gains: [[f32; N_PARTIALS]; 2],
    pan_l: f32,
    pan_r: f32,
    base_hz: f32,
    /// Control-rate drift values: pitch, cutoff, amplitude.
    drift_cache: [f32; 3],
    mod_hz_cache: f32,
    cutoff_cache: f32,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            state: VoiceState::Idle,
            note_id: 0,
            voice_id: 0,
            channel: 0,
            note: 60,
            velocity: 0.0,
            held_by_pedal: false,
            releasing: false,
            unison_x: 0.0,
            detune_jitter: 0.0,
            internal_id: 0,
            birth_block: 0,
            release_stamp: 0,
            steal_gain: 1.0,
            steal_dec: 0.0,
            osc: [Osc::default(); 2],
            mod_osc: ModOsc::default(),
            ladder: Ladder::default(),
            amp_env: Adsr::default(),
            filter_env: Adsr::default(),
            fm_env: Adsr::default(),
            lfo1: Lfo::default(),
            rng: Rng32::default(),
            drift_pitch: Drift::default(),
            drift_cutoff: Drift::default(),
            drift_amp: Drift::default(),
            pink: Pink::default(),
            jitter: [0.0; 2],
            tol: Tolerance::NEUTRAL,
            gains: [[0.0; N_PARTIALS]; 2],
            pan_l: std::f32::consts::FRAC_1_SQRT_2,
            pan_r: std::f32::consts::FRAC_1_SQRT_2,
            base_hz: 261.625_57,
            drift_cache: [0.0; 3],
            mod_hz_cache: 261.625_57,
            cutoff_cache: 12_000.0,
        }
    }
}

impl Voice {
    pub fn clear(&mut self) {
        self.state = VoiceState::Idle;
        self.releasing = false;
        self.held_by_pedal = false;
        self.steal_gain = 1.0;
        self.steal_dec = 0.0;
        self.amp_env.reset();
        self.filter_env.reset();
        self.fm_env.reset();
        self.ladder.reset();
        self.drift_pitch.reset();
        self.drift_cutoff.reset();
        self.drift_amp.reset();
        self.pink.reset();
        self.jitter = [0.0; 2];
    }

    #[inline]
    pub fn is_free(&self) -> bool {
        self.state == VoiceState::Idle
    }

    /// Peak amplitude right now, used to rank stealing candidates.
    #[inline]
    pub fn peak_amp(&self) -> f32 {
        self.amp_env.out * self.velocity * self.tol.gain
    }

    pub fn terminated(&self) -> Terminated {
        Terminated {
            voice_id: self.voice_id,
            channel: self.channel,
            note: self.note,
        }
    }

    pub fn begin_steal(&mut self, sample_rate: f32) {
        self.state = VoiceState::Stealing;
        self.steal_gain = 1.0;
        self.steal_dec = 1.0 / (STEAL_FADE_S * sample_rate).max(1.0);
    }

    pub fn start_release(&mut self, stamp: u64) {
        self.releasing = true;
        self.release_stamp = stamp;
        self.amp_env.release();
        self.filter_env.release();
        self.fm_env.release();
    }

    /// Refresh everything that is only recomputed once per control block.
    pub fn prepare_block(&mut self, b: &VoiceBlock, block_len: usize) {
        let analog_on = b.analog > 1.0e-4;
        self.lfo1
            .tick(b.lfo1_shape, b.lfo1_rate, block_len as f32 * b.inv_sample_rate);

        if analog_on {
            let dp = self.drift_pitch.tick(&mut self.rng, &b.drift_coeffs);
            let dc = self.drift_cutoff.tick(&mut self.rng, &b.drift_coeffs);
            let da = self.drift_amp.tick(&mut self.rng, &b.drift_coeffs);
            self.drift_cache = [dp, dc, da];
        } else {
            self.drift_cache = [0.0; 3];
        }

        let lfo1_env = self.lfo1.envelope(b.lfo1_delay, b.lfo1_fade);
        let lfo1 = self.lfo1.value;

        // ---- pitch -------------------------------------------------------
        let unison_detune = if b.unison > 1 {
            let x = self.unison_x;
            b.detune_cents * x.signum() * x.abs().powf(1.6) + 0.08 * b.detune_cents * self.detune_jitter
        } else {
            0.0
        };
        let vibrato = b.vibrato_cents * b.mod_wheel * lfo1 * lfo1_env;
        let pitch_lfo = b.lfo_amount(LfoDest::Pitch, lfo1, lfo1_env) * 100.0;
        let drift_cents = self.drift_cache[0] * DRIFT_PITCH_CENTS * b.analog;

        self.base_hz = 440.0
            * (((self.note as f32 - 69.0) * 100.0
                + b.global_cents
                + unison_detune
                + vibrato
                + pitch_lfo
                + drift_cents)
                / 1200.0)
                .exp2();

        // ---- panning -----------------------------------------------------
        let pan = (b.spread * self.unison_x + self.tol.pan_offset).clamp(-1.0, 1.0);
        let theta = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
        self.pan_l = theta.cos();
        self.pan_r = theta.sin();

        // ---- oscillator increments and partial gains ----------------------
        for i in 0..2 {
            let hz = self.base_hz * (b.coarse_cents[i] + self.tol.pitch_cents[i] * 1.0) .exp2_cents();
            self.osc[i].inc = hz * b.inv_sample_rate;
            self.gains[i] = osc::partial_gains(
                &b.sliders[i],
                b.tilt[i],
                b.partial_decay_db[i],
                b.odd_even[i],
                hz,
                b.nyquist,
            );
        }

        // ---- modulator ----------------------------------------------------
        let mod_hz = fm::modulator_hz(
            b.fm_mode,
            b.fm_ratio,
            b.fm_ratio_fine,
            b.fm_fixed_hz,
            self.base_hz,
        );
        self.mod_osc.inc = mod_hz * b.inv_sample_rate;
        self.mod_hz_cache = mod_hz;

        // ---- envelopes ----------------------------------------------------
        self.amp_env.set_params(
            b.amp_adsr[0], b.amp_adsr[1], b.amp_adsr[2], b.amp_adsr[3], b.amp_adsr[4],
            self.tol.env_scale[0], b.sample_rate,
        );
        self.filter_env.set_params(
            b.filter_adsr[0], b.filter_adsr[1], b.filter_adsr[2], b.filter_adsr[3],
            b.filter_adsr[4], self.tol.env_scale[1], b.sample_rate,
        );
        self.fm_env.set_params(
            b.fm_adsr[0], b.fm_adsr[1], b.fm_adsr[2], b.fm_adsr[3], b.fm_adsr[4],
            self.tol.env_scale[2], b.sample_rate,
        );

        // ---- filter cutoff, summed in semitones then exponentiated once ----
        let cutoff_lfo = b.lfo_amount(LfoDest::Cutoff, lfo1, lfo1_env) * 48.0;
        let semis = 12.0 * (b.cutoff_hz / 440.0).log2()
            + b.filter_keytrack * (self.note as f32 - 60.0)
            + b.filter_env_amount * self.filter_env.out
            + b.filter_vel_amount * self.velocity
            + cutoff_lfo
            + self.tol.cutoff_st
            + 0.40 * b.analog * self.drift_cache[1] * (DRIFT_CUTOFF_CENTS / 100.0);
        let cutoff = 440.0 * (semis / 12.0).exp2();

        let target = Ladder::gain_for(cutoff, b.sample_rate);
        self.ladder.set_target_gain(target, block_len);
        self.cutoff_cache = cutoff;
    }

    /// Render `len` samples into the stereo accumulator.
    pub fn render(&mut self, b: &VoiceBlock, out_l: &mut [f32], out_r: &mut [f32]) {
        let analog_on = b.analog > 1.0e-4;
        let k = filter::resonance_to_k(b.resonance + self.tol.resonance);
        let bass_comp = 1.0 + k * b.reso_comp * 0.25;
        let fb_cycles = fm::feedback_cycles(b.fm_feedback);
        let keytrack = fm::keytrack_scale(b.fm_keytrack, self.base_hz);
        let index_cap = fm::max_index_cycles(self.mod_hz_cache, b.sample_rate);

        let amp_drift = 1.0 + DRIFT_AMP * b.analog * self.drift_cache[2];
        let noise_gain = NOISE_FLOOR + b.noise_level * 0.25 + analog::NOISE_LEVEL * b.analog;
        let jitter_depth = if analog_on { JITTER_SIGMA * b.analog } else { 0.0 };
        let asym = VCA_ASYM * b.analog;

        for (sl, sr) in out_l.iter_mut().zip(out_r.iter_mut()) {
            let amp_env = self.amp_env.next();
            let fm_env = self.fm_env.next();
            self.filter_env.next();

            let modulation = self.mod_osc.next(fb_cycles);
            let env_mult = fm::env_multiplier(b.fm_env_amount, fm_env);

            let mut voice_out = 0.0;
            for i in 0..2 {
                let index = (b.fm_index[i] * env_mult * keytrack).min(index_cap);
                let mut pm = index * modulation;

                if jitter_depth > 0.0 {
                    let white = self.rng.bipolar();
                    self.jitter[i] += b.jitter_coeff * (white - self.jitter[i]);
                    pm += jitter_depth * self.jitter[i];
                }

                let width = (b.pulse_width[i] + self.tol.pulse_width[i]).clamp(0.02, 0.98);
                voice_out += self.osc[i].next(b.wave[i], width, pm, b.blend[i], &self.gains[i])
                    * b.level[i];
            }

            voice_out += self.pink.next(&mut self.rng) * noise_gain;

            let driven = tanh_fast(voice_out * b.drive_gain * bass_comp / DRIVE_HEADROOM)
                * DRIVE_HEADROOM;
            let filtered = self.ladder.next(driven, k, b.reso_comp, &b.filter_taps);

            let gain = amp_env * self.velocity * self.tol.gain * amp_drift * self.steal_gain;
            let pre = filtered * gain;
            // Asymmetric, so this adds a second harmonic - the warm one. The
            // DC it also produces is why the output stage has a DC blocker.
            let shaped = if analog_on {
                tanh_fast(pre + asym * pre * pre)
            } else {
                pre
            };

            *sl += shaped * self.pan_l;
            *sr += shaped * self.pan_r;

            if self.state == VoiceState::Stealing {
                self.steal_gain -= self.steal_dec;
                if self.steal_gain <= 0.0 {
                    self.steal_gain = 0.0;
                }
            }
        }
    }

    /// Has this voice gone silent for good?
    #[inline]
    pub fn is_done(&self) -> bool {
        match self.state {
            VoiceState::Idle => false,
            VoiceState::Stealing => self.steal_gain <= 0.0,
            VoiceState::Active => self.amp_env.stage == EnvStage::Finished,
        }
    }

    pub fn cutoff_hz(&self) -> f32 {
        self.cutoff_cache
    }
}

/// Small helper so pitch offsets read as cents at the call site.
trait Cents {
    fn exp2_cents(self) -> f32;
}
impl Cents for f32 {
    #[inline(always)]
    fn exp2_cents(self) -> f32 {
        (self / 1200.0).exp2()
    }
}

// ---------------------------------------------------------------------------
// Voice manager
// ---------------------------------------------------------------------------

/// How a sounding unison group ranks as a stealing candidate. Lower steals
/// first, and the tiebreak is compared second.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
struct StealKey {
    class: u8,
    tiebreak: f32,
}

pub struct VoiceManager {
    pub voices: Vec<Voice>,
    pub terminated: Vec<Terminated>,
    next_note_id: u32,
    next_internal_id: u64,
    /// Sub-voices the user has made available, excluding the declick reserve.
    pub capacity: usize,
    pub sample_rate: f32,
    /// Incremented once per `process()` call.
    pub block_stamp: u64,
}

impl Default for VoiceManager {
    fn default() -> Self {
        let mut voices = Vec::with_capacity(VOICE_SLOTS);
        voices.resize_with(VOICE_SLOTS, Voice::default);
        Self {
            voices,
            terminated: Vec::with_capacity(VOICE_SLOTS),
            next_note_id: 1,
            next_internal_id: 1,
            capacity: 16,
            sample_rate: 48_000.0,
            block_stamp: 0,
        }
    }
}

impl VoiceManager {
    pub fn reset(&mut self) {
        for v in &mut self.voices {
            v.clear();
        }
        self.terminated.clear();
    }

    pub fn active_count(&self) -> usize {
        self.voices.iter().filter(|v| !v.is_free()).count()
    }

    /// Voices that may hold a new note, i.e. everything but the declick spares.
    #[inline]
    fn pool(&self) -> usize {
        self.capacity.min(VOICE_SLOTS - DECLICK_VOICES)
    }

    /// Fallback ID for hosts that do not supply one. Polyphonic modulation
    /// will not work in that case, but notes still play.
    #[inline]
    const fn fallback_voice_id(note: u8, channel: u8) -> i32 {
        note as i32 | ((channel as i32) << 16)
    }

    fn free_slot(&self, allow_reserve: bool) -> Option<usize> {
        let limit = if allow_reserve {
            VOICE_SLOTS
        } else {
            self.pool()
        };
        (0..limit).find(|&i| self.voices[i].is_free())
    }

    fn free_count(&self) -> usize {
        (0..self.pool()).filter(|&i| self.voices[i].is_free()).count()
    }

    fn steal_key(&self, note_id: u32) -> StealKey {
        let mut peak = 0.0f32;
        let mut oldest_release = u64::MAX;
        let mut oldest_start = u64::MAX;
        let mut any = false;
        let mut all_releasing = true;

        for v in self.voices.iter().filter(|v| v.note_id == note_id && !v.is_free()) {
            any = true;
            peak = peak.max(v.peak_amp());
            oldest_start = oldest_start.min(v.internal_id);
            if v.releasing {
                oldest_release = oldest_release.min(v.release_stamp);
            } else {
                all_releasing = false;
            }
        }

        if !any {
            return StealKey { class: 0, tiebreak: 0.0 };
        }
        let class = if all_releasing {
            1
        } else if peak < QUIET_THRESHOLD {
            2
        } else {
            3
        };

        let tiebreak = match class {
            1 => oldest_release as f32,
            2 => peak,
            _ => oldest_start as f32,
        };
        StealKey { class, tiebreak }
    }

    fn group_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self
            .voices
            .iter()
            .filter(|v| v.state == VoiceState::Active)
            .map(|v| v.note_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Free at least `needed` slots by fading out whole unison groups. Groups
    /// are stolen entire; an orphaned sub-voice is an audible detuned ghost.
    fn make_room(&mut self, needed: usize, exclude: u32) {
        let mut guard = 0;
        while self.free_count() < needed && guard < VOICE_SLOTS {
            guard += 1;

            let mut best: Option<(u32, StealKey)> = None;
            for id in self.group_ids() {
                if id == exclude {
                    continue;
                }
                // Never steal a group that only just started; a fast arpeggio
                // would otherwise eat its own attack transients.
                let too_young = self
                    .voices
                    .iter()
                    .any(|v| v.note_id == id && !v.is_free() && v.birth_block == self.block_stamp);
                if too_young {
                    continue;
                }
                let key = self.steal_key(id);
                if best.is_none_or(|(_, b)| (key.class, key.tiebreak) < (b.class, b.tiebreak)) {
                    best = Some((id, key));
                }
            }

            let Some((victim, _)) = best else { break };
            let sample_rate = self.sample_rate;
            for v in self.voices.iter_mut().filter(|v| v.note_id == victim && !v.is_free()) {
                v.begin_steal(sample_rate);
            }
            // A stolen voice's slot is occupied until its fade finishes, so
            // the replacement note comes out of the declick reserve instead of
            // waiting. That is what makes stealing click-free and instant.
            if self.free_count() < needed && self.free_slot(true).is_none() {
                break;
            }
            if self.free_count() >= needed {
                break;
            }
            if guard > 8 {
                break;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn note_on(
        &mut self,
        voice_id: Option<i32>,
        channel: u8,
        note: u8,
        velocity: f32,
        unison: usize,
        analog: f32,
        phase_random: f32,
    ) {
        let unison = unison.clamp(1, MAX_UNISON).min(self.pool());
        let note_id = self.next_note_id;
        self.next_note_id = self.next_note_id.wrapping_add(1);
        self.make_room(unison, note_id);

        let resolved_id = voice_id.unwrap_or_else(|| Self::fallback_voice_id(note, channel));
        let sample_rate = self.sample_rate;

        for i in 0..unison {
            let Some(slot) = self.free_slot(true) else { break };
            let internal = self.next_internal_id;
            self.next_internal_id = self.next_internal_id.wrapping_add(1);

            let seed = 0x9E37_79B9_u32
                .wrapping_mul(note_id)
                .wrapping_add((i as u32).wrapping_mul(0x85EB_CA6B))
                .wrapping_add(note as u32);
            let mut rng = Rng32::new(seed);
            let tol = Tolerance::draw(&mut rng, analog);

            let v = &mut self.voices[slot];
            v.clear();
            v.state = VoiceState::Active;
            v.note_id = note_id;
            v.voice_id = resolved_id;
            v.channel = channel;
            v.note = note;
            v.velocity = velocity.clamp(0.0, 1.0).sqrt();
            v.internal_id = internal;
            v.birth_block = self.block_stamp;
            v.tol = tol;
            v.rng = rng;

            v.unison_x = if unison == 1 {
                0.0
            } else {
                2.0 * i as f32 / (unison as f32 - 1.0) - 1.0
            };
            v.detune_jitter = v.rng.bipolar();

            // Random start phase. Without it a unison stack peaks at n times
            // amplitude on the first sample and only separates after 1/df
            // seconds - about 0.8 s at 10 cents on A3.
            let r = phase_random.clamp(0.0, 1.0);
            v.osc[0].reset(v.rng.unipolar() * r);
            v.osc[1].reset(v.rng.unipolar() * r);
            v.mod_osc.reset(v.rng.unipolar() * r);
            v.lfo1.reset(0.0, seed ^ 0x5BF0_3635);
            v.ladder.mismatch = tol.stage_mismatch;
            v.ladder.snap_gain(Ladder::gain_for(12_000.0, sample_rate));

            v.amp_env.trigger();
            v.filter_env.trigger();
            v.fm_env.trigger();
        }
    }

    /// Match on voice ID first, falling back to channel and note - hosts do
    /// not always supply an ID on note-off.
    pub fn note_off(&mut self, voice_id: Option<i32>, channel: u8, note: u8, sustain: bool) {
        let now = self.next_internal_id;
        self.next_internal_id = self.next_internal_id.wrapping_add(1);

        for v in self.voices.iter_mut() {
            if v.state != VoiceState::Active {
                continue;
            }
            let matches = match voice_id {
                Some(id) => v.voice_id == id,
                None => v.channel == channel && v.note == note,
            };
            if !matches {
                continue;
            }
            if sustain {
                v.held_by_pedal = true;
            } else {
                v.start_release(now);
            }
        }
    }

    pub fn release_sustained(&mut self) {
        let now = self.next_internal_id;
        self.next_internal_id = self.next_internal_id.wrapping_add(1);
        for v in self.voices.iter_mut() {
            if v.held_by_pedal {
                v.held_by_pedal = false;
                v.start_release(now);
            }
        }
    }

    /// Immediate stop, no fade - that is what choke means.
    pub fn choke(&mut self, voice_id: Option<i32>, channel: u8, note: u8) {
        for v in self.voices.iter_mut() {
            if v.is_free() {
                continue;
            }
            let matches = match voice_id {
                Some(id) => v.voice_id == id,
                None => v.channel == channel && v.note == note,
            };
            if matches {
                self.terminated.push(v.terminated());
                v.clear();
            }
        }
    }

    /// Free finished voices and record their IDs for the host.
    pub fn collect_finished(&mut self) {
        for v in self.voices.iter_mut() {
            if !v.is_free() && v.is_done() {
                self.terminated.push(v.terminated());
                v.clear();
            }
        }
    }
}
