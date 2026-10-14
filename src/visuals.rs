//! Lock-free state shared from the audio thread to the editor.
//!
//! Two transports, split by payload size. Scalars go through relaxed atomics -
//! tearing between them is invisible at 60-odd frames a second. The scope
//! frame goes through a triple buffer, because a half-updated waveform reads
//! as a glitch and because the GUI only ever wants the newest complete frame.
//!
//! Nothing here ever locks on the audio thread.

use atomic_float::AtomicF32;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use crate::MAX_VOICES;

/// Horizontal resolution of the oscilloscope.
pub const SCOPE_COLUMNS: usize = 512;
/// Visible time window, chosen so a bass note still shows a couple of cycles.
pub const SCOPE_WINDOW_S: f32 = 0.021;

const RELAXED: Ordering = Ordering::Relaxed;

pub struct VisualState {
    /// Amplitude envelope level per voice slot; 0 means the slot is free.
    pub voice_env: [AtomicF32; MAX_VOICES],
    /// MIDI note per slot, 255 when free.
    pub voice_note: [AtomicU8; MAX_VOICES],
    /// Number of slots currently sounding.
    pub active_voices: AtomicU32,
    /// Slots the user has made available, for the header readout.
    pub voice_capacity: AtomicU32,

    /// Cutoff actually applied to the most recent voice, after envelope,
    /// keytrack, velocity and drift. This is what makes the filter display
    /// show the modulation rather than just the knob.
    pub filter_cutoff_hz: AtomicF32,
    pub filter_resonance: AtomicF32,

    pub amp_env_level: AtomicF32,
    pub filter_env_level: AtomicF32,
    pub amp_env_stage: AtomicU8,
    pub filter_env_stage: AtomicU8,

    pub lfo1_value: AtomicF32,
    pub lfo2_value: AtomicF32,
    pub chorus_lfo: AtomicF32,

    pub peak_l: AtomicF32,
    pub peak_r: AtomicF32,

    pub sample_rate: AtomicF32,
}

impl Default for VisualState {
    fn default() -> Self {
        Self {
            voice_env: std::array::from_fn(|_| AtomicF32::new(0.0)),
            voice_note: std::array::from_fn(|_| AtomicU8::new(255)),
            active_voices: AtomicU32::new(0),
            voice_capacity: AtomicU32::new(16),
            filter_cutoff_hz: AtomicF32::new(12_000.0),
            filter_resonance: AtomicF32::new(0.0),
            amp_env_level: AtomicF32::new(0.0),
            filter_env_level: AtomicF32::new(0.0),
            amp_env_stage: AtomicU8::new(0),
            filter_env_stage: AtomicU8::new(0),
            lfo1_value: AtomicF32::new(0.0),
            lfo2_value: AtomicF32::new(0.0),
            chorus_lfo: AtomicF32::new(0.0),
            peak_l: AtomicF32::new(0.0),
            peak_r: AtomicF32::new(0.0),
            sample_rate: AtomicF32::new(48_000.0),
        }
    }
}

impl VisualState {
    /// Zero everything, so a stopped transport does not leave frozen LEDs lit.
    pub fn clear(&self) {
        for (env, note) in self.voice_env.iter().zip(self.voice_note.iter()) {
            env.store(0.0, RELAXED);
            note.store(255, RELAXED);
        }
        self.active_voices.store(0, RELAXED);
        self.amp_env_level.store(0.0, RELAXED);
        self.filter_env_level.store(0.0, RELAXED);
        self.amp_env_stage.store(0, RELAXED);
        self.filter_env_stage.store(0, RELAXED);
        self.peak_l.store(0.0, RELAXED);
        self.peak_r.store(0.0, RELAXED);
    }

    #[inline]
    pub fn store_peaks(&self, l: f32, r: f32) {
        // Decay on the audio thread so the meter falls smoothly regardless of
        // how often the editor happens to redraw.
        let decay = 0.94;
        let prev_l = self.peak_l.load(RELAXED) * decay;
        let prev_r = self.peak_r.load(RELAXED) * decay;
        self.peak_l.store(prev_l.max(l), RELAXED);
        self.peak_r.store(prev_r.max(r), RELAXED);
    }
}

/// One complete scope frame. A min/max envelope rather than raw samples, so
/// the display stays honest under heavy decimation instead of aliasing.
#[derive(Clone, Copy)]
pub struct ScopeFrame {
    pub min: [f32; SCOPE_COLUMNS],
    pub max: [f32; SCOPE_COLUMNS],
}

impl Default for ScopeFrame {
    fn default() -> Self {
        Self {
            min: [0.0; SCOPE_COLUMNS],
            max: [0.0; SCOPE_COLUMNS],
        }
    }
}

pub type ScopeOutput = triple_buffer::Output<ScopeFrame>;

/// Audio-thread half of the scope. Lives in the plugin struct and is never
/// shared, so it needs no synchronisation of its own.
pub struct ScopeInput {
    buffer: triple_buffer::Input<ScopeFrame>,
    samples_per_column: u32,
    column: usize,
    in_column: u32,
    cur_min: f32,
    cur_max: f32,
    prev: f32,
    /// Waiting for a rising zero crossing before starting the next frame,
    /// which is what keeps the displayed waveform from sliding sideways.
    armed: bool,
    /// Give up waiting for a trigger after this many samples so quiet or
    /// DC-free material still refreshes.
    since_trigger: u32,
    trigger_timeout: u32,
}

impl ScopeInput {
    pub fn new() -> (Self, ScopeOutput) {
        let (buffer, output) = triple_buffer::TripleBuffer::new(&ScopeFrame::default()).split();
        (
            Self {
                buffer,
                samples_per_column: 2,
                column: 0,
                in_column: 0,
                cur_min: f32::INFINITY,
                cur_max: f32::NEG_INFINITY,
                prev: 0.0,
                armed: true,
                since_trigger: 0,
                trigger_timeout: 48_000 / 10,
            },
            output,
        )
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.samples_per_column =
            ((sample_rate * SCOPE_WINDOW_S) / SCOPE_COLUMNS as f32).round().max(1.0) as u32;
        self.trigger_timeout = (sample_rate / 10.0) as u32;
    }

    pub fn reset(&mut self) {
        self.column = 0;
        self.in_column = 0;
        self.cur_min = f32::INFINITY;
        self.cur_max = f32::NEG_INFINITY;
        self.prev = 0.0;
        self.armed = true;
        self.since_trigger = 0;
    }

    #[inline(always)]
    pub fn push(&mut self, sample: f32) {
        if self.armed {
            self.since_trigger += 1;
            let rising = self.prev <= 0.0 && sample > 0.0;
            self.prev = sample;
            if !rising && self.since_trigger < self.trigger_timeout {
                return;
            }
            self.armed = false;
            self.since_trigger = 0;
        }
        self.prev = sample;

        self.cur_min = self.cur_min.min(sample);
        self.cur_max = self.cur_max.max(sample);
        self.in_column += 1;
        if self.in_column < self.samples_per_column {
            return;
        }

        // `input_buffer()` hands back the write side directly, so there is no
        // intermediate copy. Safe here only because every column is written
        // before each publish.
        let frame = self.buffer.input_buffer();
        frame.min[self.column] = self.cur_min;
        frame.max[self.column] = self.cur_max;

        self.cur_min = f32::INFINITY;
        self.cur_max = f32::NEG_INFINITY;
        self.in_column = 0;
        self.column += 1;

        if self.column >= SCOPE_COLUMNS {
            self.buffer.publish();
            self.column = 0;
            self.armed = true;
        }
    }
}
