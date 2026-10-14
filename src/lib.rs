//! Drift - a polyphonic hybrid additive/subtractive synthesizer.

use nih_plug::prelude::*;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

pub mod analog;
pub mod editor;
pub mod env;
pub mod filter;
pub mod fm;
pub mod fx;
pub mod osc;
pub mod params;
pub mod visuals;
pub mod voice;

use analog::{DenormalGuard, Drift as DriftGen, DriftCoeffs, Rng32, GLOBAL_DRIFT_CENTS};
use env::Lfo;
use fx::{Chorus, DcBlocker, OnePoleLp};
use params::SynthParams;
use visuals::{ScopeInput, ScopeOutput, VisualState};
use voice::{VoiceBlock, VoiceManager};

/// Harmonic drawbars per oscillator.
pub const N_PARTIALS: usize = 8;
/// Largest sub-voice pool the user can select.
pub const MAX_VOICES: usize = 32;
/// Extra slots reserved for the fade-out of stolen voices.
pub const DECLICK_VOICES: usize = 4;
/// Total voice slots.
pub const VOICE_SLOTS: usize = MAX_VOICES + DECLICK_VOICES;
/// Maximum unison sub-voices per note.
pub const MAX_UNISON: usize = 7;
/// Control-rate sub-block length in samples.
pub const CTL_BLOCK: usize = 32;

/// Corner of the analog output tilt at `analog == 0` and `analog == 1`.
const HF_TILT_CLEAN: f32 = 20_000.0;
const HF_TILT_ANALOG: f32 = 14_000.0;

pub struct Drift {
    params: Arc<SynthParams>,
    voices: VoiceManager,

    sample_rate: f32,
    drift_coeffs: DriftCoeffs,
    jitter_coeff: f32,

    lfo2: Lfo,
    global_drift: DriftGen,
    global_rng: Rng32,
    global_drift_value: f32,

    chorus: Chorus,
    dc_l: DcBlocker,
    dc_r: DcBlocker,
    hf_l: OnePoleLp,
    hf_r: OnePoleLp,
    hf_hz: f32,

    pitch_bend: f32,
    mod_wheel: f32,
    sustain_pedal: bool,

    visuals: Arc<VisualState>,
    scope: ScopeInput,
    scope_output: Arc<Mutex<ScopeOutput>>,
}

impl Default for Drift {
    fn default() -> Self {
        let (scope, scope_output) = ScopeInput::new();
        Self {
            params: Arc::new(SynthParams::default()),
            voices: VoiceManager::default(),
            sample_rate: 48_000.0,
            drift_coeffs: DriftCoeffs::new(48_000.0 / CTL_BLOCK as f32),
            jitter_coeff: 0.178,
            lfo2: Lfo::default(),
            global_drift: DriftGen::default(),
            global_rng: Rng32::new(0x1234_5678),
            global_drift_value: 0.0,
            chorus: Chorus::default(),
            dc_l: DcBlocker::default(),
            dc_r: DcBlocker::default(),
            hf_l: OnePoleLp::default(),
            hf_r: OnePoleLp::default(),
            hf_hz: HF_TILT_CLEAN,
            pitch_bend: 0.0,
            mod_wheel: 0.0,
            sustain_pedal: false,
            visuals: Arc::new(VisualState::default()),
            scope,
            scope_output: Arc::new(Mutex::new(scope_output)),
        }
    }
}

impl Drift {
    /// Resolve every parameter the voices need for one control block.
    fn resolve_block(&mut self, block_len: usize) -> VoiceBlock {
        let p = &self.params;
        let steps = block_len as u32;
        let sample_rate = self.sample_rate;
        let inv_sample_rate = 1.0 / sample_rate;

        let analog = p.voice.analog.smoothed.next_step(steps);

        // One global drift on master tune. Per-voice drift alone reads as
        // chorus; this is what makes the whole patch breathe together.
        self.global_drift_value = if analog > 1.0e-4 {
            self.global_drift.tick(&mut self.global_rng, &self.drift_coeffs)
        } else {
            0.0
        };

        let lfo2_rate = p.lfo2.rate.smoothed.next_step(steps);
        let lfo2_value = self.lfo2.tick(
            p.lfo2.shape.value(),
            lfo2_rate,
            block_len as f32 * inv_sample_rate,
        );

        let bend_cents = self.pitch_bend * p.voice.bend_range.value() as f32 * 100.0;
        let global_cents = p.master.tune.smoothed.next_step(steps)
            + bend_cents
            + self.global_drift_value * GLOBAL_DRIFT_CENTS * analog;

        let osc_block = |o: &params::OscParams, steps: u32| {
            let mut sliders = [0.0_f32; N_PARTIALS];
            for (k, slot) in sliders.iter_mut().enumerate() {
                *slot = o.partials[k].amp.smoothed.next_step(steps);
            }
            (
                o.wave.value(),
                o.coarse.value() as f32 * 100.0 + o.fine.smoothed.next_step(steps),
                o.pulse_width.smoothed.next_step(steps),
                o.level.smoothed.next_step(steps),
                o.blend.smoothed.next_step(steps),
                sliders,
                o.tilt.smoothed.next_step(steps),
                o.partial_decay.smoothed.next_step(steps),
                o.odd_even.smoothed.next_step(steps),
            )
        };
        let a = osc_block(&p.osc1, steps);
        let b = osc_block(&p.osc2, steps);

        let adsr = |e: &params::AdsrParams, steps: u32| {
            [
                e.attack.smoothed.next_step(steps),
                e.decay.smoothed.next_step(steps),
                e.sustain.smoothed.next_step(steps),
                e.release.smoothed.next_step(steps),
                e.curve.value(),
            ]
        };

        let filter_mode = p.filter.mode.value();

        VoiceBlock {
            sample_rate,
            nyquist: sample_rate * 0.5,
            inv_sample_rate,

            wave: [a.0, b.0],
            coarse_cents: [a.1, b.1],
            pulse_width: [a.2, b.2],
            level: [a.3, b.3],
            blend: [a.4, b.4],
            sliders: [a.5, b.5],
            tilt: [a.6, b.6],
            partial_decay_db: [a.7, b.7],
            odd_even: [a.8, b.8],

            fm_mode: p.fm.mode.value(),
            fm_ratio: p.fm.ratio.smoothed.next_step(steps),
            fm_ratio_fine: p.fm.ratio_fine.smoothed.next_step(steps),
            fm_fixed_hz: p.fm.fixed_hz.smoothed.next_step(steps),
            fm_index: [
                p.fm.index1.smoothed.next_step(steps),
                p.fm.index2.smoothed.next_step(steps),
            ],
            fm_feedback: p.fm.feedback.smoothed.next_step(steps),
            fm_env_amount: p.fm.env_amount.smoothed.next_step(steps),
            fm_keytrack: p.fm.keytrack.value(),

            filter_mode,
            filter_taps: filter::mode_taps(filter_mode),
            cutoff_hz: p.filter.cutoff.smoothed.next_step(steps),
            resonance: p.filter.resonance.smoothed.next_step(steps),
            drive_gain: util::db_to_gain(p.filter.drive.smoothed.next_step(steps)),
            filter_keytrack: p.filter.keytrack.value(),
            filter_env_amount: p.filter.env_amount.smoothed.next_step(steps),
            filter_vel_amount: p.filter.vel_amount.value(),
            reso_comp: p.filter.reso_comp.smoothed.next_step(steps),

            amp_adsr: adsr(&p.amp_env, steps),
            filter_adsr: adsr(&p.filter_env, steps),
            fm_adsr: adsr(&p.fm_env, steps),

            unison: p.voice.unison.value() as usize,
            detune_cents: p.voice.detune.smoothed.next_step(steps),
            spread: p.voice.spread.smoothed.next_step(steps),
            analog,
            noise_level: p.master.noise.smoothed.next_step(steps),

            global_cents,
            lfo1_shape: p.lfo1.shape.value(),
            lfo1_rate: p.lfo1.rate.smoothed.next_step(steps),
            lfo1_depth: p.lfo1.depth.smoothed.next_step(steps),
            lfo1_dest: p.lfo1.dest.value(),
            lfo1_delay: p.lfo1.delay.value(),
            lfo1_fade: p.lfo1.fade.value(),
            vibrato_cents: p.voice.vibrato.smoothed.next_step(steps),
            mod_wheel: self.mod_wheel,

            lfo2_value,
            lfo2_dest: p.lfo2.dest.value(),
            lfo2_depth: p.lfo2.depth.smoothed.next_step(steps),

            drift_coeffs: self.drift_coeffs,
            jitter_coeff: self.jitter_coeff,
        }
    }

    fn handle_event(&mut self, event: NoteEvent<()>) {
        match event {
            NoteEvent::NoteOn {
                voice_id,
                channel,
                note,
                velocity,
                ..
            } => {
                let p = &self.params;
                self.voices.capacity = p.voice.voice_count.value().count();
                self.voices.note_on(
                    voice_id,
                    channel,
                    note,
                    velocity,
                    p.voice.unison.value() as usize,
                    p.voice.analog.smoothed.previous_value(),
                    p.voice.phase_random.value(),
                );
            }
            NoteEvent::NoteOff {
                voice_id,
                channel,
                note,
                ..
            } => self
                .voices
                .note_off(voice_id, channel, note, self.sustain_pedal),
            NoteEvent::Choke {
                voice_id,
                channel,
                note,
                ..
            } => self.voices.choke(voice_id, channel, note),
            NoteEvent::MidiPitchBend { value, .. } => {
                self.pitch_bend = value * 2.0 - 1.0;
            }
            NoteEvent::MidiCC { cc, value, .. } => match cc {
                1 => self.mod_wheel = value,
                64 => {
                    let down = value >= 0.5;
                    self.sustain_pedal = down;
                    if !down {
                        self.voices.release_sustained();
                    }
                }
                _ => (),
            },
            _ => (),
        }
    }

    /// Publish everything the editor draws. Roughly two dozen relaxed stores
    /// plus one scope sample per frame; no locks, no allocation.
    fn update_visuals(&mut self, left: &[f32], right: &[f32]) {
        use std::sync::atomic::Ordering::Relaxed;

        let v = &self.visuals;
        let mut active = 0u32;
        let mut newest: Option<&voice::Voice> = None;

        for (i, slot) in self.voices.voices.iter().take(MAX_VOICES).enumerate() {
            if slot.is_free() {
                v.voice_env[i].store(0.0, Relaxed);
                v.voice_note[i].store(255, Relaxed);
            } else {
                active += 1;
                v.voice_env[i].store(slot.amp_env.out, Relaxed);
                v.voice_note[i].store(slot.note, Relaxed);
                if newest.is_none_or(|n| slot.internal_id > n.internal_id) {
                    newest = Some(slot);
                }
            }
        }
        v.active_voices.store(active, Relaxed);
        v.voice_capacity
            .store(self.params.voice.voice_count.value().count() as u32, Relaxed);

        if let Some(n) = newest {
            v.filter_cutoff_hz.store(n.cutoff_hz(), Relaxed);
            v.amp_env_level.store(n.amp_env.out, Relaxed);
            v.filter_env_level.store(n.filter_env.out, Relaxed);
            v.amp_env_stage.store(n.amp_env.stage.as_u8(), Relaxed);
            v.filter_env_stage.store(n.filter_env.stage.as_u8(), Relaxed);
        }
        v.filter_resonance
            .store(self.params.filter.resonance.smoothed.previous_value(), Relaxed);
        v.lfo1_value.store(
            newest.map(|n| n.lfo1.value).unwrap_or(0.0),
            Relaxed,
        );
        v.lfo2_value.store(self.lfo2.value, Relaxed);

        let mut peak_l = 0.0f32;
        let mut peak_r = 0.0f32;
        for (&l, &r) in left.iter().zip(right.iter()) {
            peak_l = peak_l.max(l.abs());
            peak_r = peak_r.max(r.abs());
            self.scope.push(0.5 * (l + r));
        }
        v.store_peaks(peak_l, peak_r);
    }

    /// Global output chain: DC block, chorus, analog tilt, gain, soft limit.
    fn render_output(&mut self, left: &mut [f32], right: &mut [f32], block_len: usize) {
        let p = &self.params;
        let steps = block_len as u32;

        let mode = p.chorus.mode.value();
        let rate = p.chorus.rate.smoothed.next_step(steps);
        let depth = p.chorus.depth.smoothed.next_step(steps);
        let feedback = p.chorus.feedback.smoothed.next_step(steps);
        let mix = p.chorus.mix.smoothed.next_step(steps);
        let gain = p.master.gain.smoothed.next_step(steps);
        let analog = p.voice.analog.smoothed.previous_value();

        let target_hf = HF_TILT_CLEAN - (HF_TILT_CLEAN - HF_TILT_ANALOG) * analog;
        if (target_hf - self.hf_hz).abs() > 1.0 {
            self.hf_hz = target_hf;
            self.hf_l.set_cutoff(self.hf_hz, self.sample_rate);
            self.hf_r.set_cutoff(self.hf_hz, self.sample_rate);
        }

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mut dl = self.dc_l.next(*l);
            let mut dr = self.dc_r.next(*r);

            let (cl, cr) = self.chorus.next(dl, dr, mode, rate, depth, feedback, mix);
            dl = self.hf_l.next(cl);
            dr = self.hf_r.next(cr);

            *l = fx::limit(dl * gain);
            *r = fx::limit(dr * gain);
        }
    }
}

impl Plugin for Drift {
    const NAME: &'static str = "Drift";
    const VENDOR: &'static str = "Fonsie Audio";
    const URL: &'static str = "https://example.com/drift";
    const EMAIL: &'static str = "beat.weber@bithawk.ch";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: None,
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const MIDI_INPUT: MidiConfig = MidiConfig::MidiCCs;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        editor::create(
            self.params.clone(),
            self.visuals.clone(),
            self.scope_output.clone(),
        )
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        self.sample_rate = buffer_config.sample_rate;
        self.voices.sample_rate = self.sample_rate;
        self.drift_coeffs = DriftCoeffs::new(self.sample_rate / CTL_BLOCK as f32);
        // One-pole at 1.5 kHz for the oscillator phase jitter.
        self.jitter_coeff =
            1.0 - (-std::f32::consts::TAU * 1500.0 / self.sample_rate).exp();

        self.scope.set_sample_rate(self.sample_rate);
        self.visuals.sample_rate.store(self.sample_rate, std::sync::atomic::Ordering::Relaxed);

        self.chorus.set_sample_rate(self.sample_rate);
        self.dc_l.set_sample_rate(self.sample_rate);
        self.dc_r.set_sample_rate(self.sample_rate);
        self.hf_l.set_cutoff(HF_TILT_CLEAN, self.sample_rate);
        self.hf_r.set_cutoff(HF_TILT_CLEAN, self.sample_rate);

        self.reset();
        true
    }

    fn reset(&mut self) {
        self.voices.reset();
        self.scope.reset();
        self.visuals.clear();
        self.chorus.reset();
        self.dc_l.reset();
        self.dc_r.reset();
        self.hf_l.reset();
        self.hf_r.reset();
        self.global_drift.reset();
        self.lfo2.reset(0.0, 0x2BAD_F00D);
        self.pitch_bend = 0.0;
        self.mod_wheel = 0.0;
        self.sustain_pedal = false;
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let _denormals = DenormalGuard::new();

        self.voices.block_stamp = self.voices.block_stamp.wrapping_add(1);
        self.voices.terminated.clear();

        let num_samples = buffer.samples();
        let channels = buffer.as_slice();
        let (left_slice, right_slice) = channels.split_at_mut(1);
        let left = &mut left_slice[0];
        let right = &mut right_slice[0];

        let mut next_event = context.next_event();
        let mut block_start = 0usize;

        while block_start < num_samples {
            let mut block_end = (block_start + CTL_BLOCK).min(num_samples);

            // Consume every event at or before the block start, then cut the
            // block short at the next one so note-ons land on the right sample.
            'events: loop {
                match next_event {
                    Some(event) if (event.timing() as usize) <= block_start => {
                        self.handle_event(event);
                        next_event = context.next_event();
                    }
                    Some(event) if (event.timing() as usize) < block_end => {
                        block_end = event.timing() as usize;
                        break 'events;
                    }
                    _ => break 'events,
                }
            }

            let block_len = block_end - block_start;
            if block_len == 0 {
                block_start = block_end;
                continue;
            }

            left[block_start..block_end].fill(0.0);
            right[block_start..block_end].fill(0.0);

            let vb = self.resolve_block(block_len);
            for v in self.voices.voices.iter_mut() {
                if v.is_free() {
                    continue;
                }
                v.prepare_block(&vb, block_len);
                v.render(
                    &vb,
                    &mut left[block_start..block_end],
                    &mut right[block_start..block_end],
                );
            }

            self.voices.collect_finished();
            self.render_output(
                &mut left[block_start..block_end],
                &mut right[block_start..block_end],
                block_len,
            );

            // Only pay for visualisation when someone is actually looking.
            if self.params.editor_state.is_open() {
                self.update_visuals(
                    &left[block_start..block_end],
                    &right[block_start..block_end],
                );
            }

            for t in self.voices.terminated.drain(..) {
                context.send_event(NoteEvent::VoiceTerminated {
                    timing: block_end as u32,
                    voice_id: Some(t.voice_id),
                    channel: t.channel,
                    note: t.note,
                });
            }

            block_start = block_end;
        }

        ProcessStatus::KeepAlive
    }
}

impl ClapPlugin for Drift {
    const CLAP_ID: &'static str = "ch.fonsie.drift";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("A polyphonic hybrid additive/subtractive synthesizer with analog character");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::Instrument,
        ClapFeature::Synthesizer,
        ClapFeature::Stereo,
    ];
    const CLAP_POLY_MODULATION_CONFIG: Option<PolyModulationConfig> = Some(PolyModulationConfig {
        max_voice_capacity: MAX_VOICES as u32,
        supports_overlapping_voices: true,
    });
}

impl Vst3Plugin for Drift {
    const VST3_CLASS_ID: [u8; 16] = *b"FonsieDriftSynth";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[
        Vst3SubCategory::Instrument,
        Vst3SubCategory::Synth,
        Vst3SubCategory::Stereo,
    ];
}

nih_export_clap!(Drift);
nih_export_vst3!(Drift);
