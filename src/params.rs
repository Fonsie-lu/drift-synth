//! All plugin parameters.
//!
//! The parameter IDs here are part of the plugin's saved state, so renaming or
//! re-ranging anything in this file will break existing projects.

use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use std::sync::atomic::AtomicU8;
use std::sync::Arc;

use crate::N_PARTIALS;

// ---------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waveform {
    #[id = "sin"]
    Sine,
    #[id = "tri"]
    Triangle,
    #[id = "saw"]
    Saw,
    #[id = "pul"]
    Pulse,
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FmRatioMode {
    /// Ratio snapped to the harmonic series.
    #[id = "harm"]
    Harmonic,
    /// Continuously variable ratio.
    #[id = "free"]
    Free,
    /// Absolute frequency, independent of the played note.
    #[id = "fixed"]
    Fixed,
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    #[id = "lp24"]
    #[name = "LP 24"]
    Lp24,
    #[id = "lp12"]
    #[name = "LP 12"]
    Lp12,
    #[id = "hp24"]
    #[name = "HP 24"]
    Hp24,
    #[id = "hp12"]
    #[name = "HP 12"]
    Hp12,
    #[id = "bp24"]
    #[name = "BP 24"]
    Bp24,
    #[id = "bp12"]
    #[name = "BP 12"]
    Bp12,
    #[id = "notch"]
    Notch,
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfoShape {
    #[id = "sin"]
    Sine,
    #[id = "tri"]
    Triangle,
    #[id = "up"]
    #[name = "Saw Up"]
    SawUp,
    #[id = "down"]
    #[name = "Saw Down"]
    SawDown,
    #[id = "sqr"]
    Square,
    #[id = "sh"]
    #[name = "S&H"]
    SampleHold,
    #[id = "rnd"]
    #[name = "Random"]
    SmoothRandom,
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfoDest {
    #[id = "off"]
    Off,
    #[id = "pitch"]
    Pitch,
    #[id = "cutoff"]
    Cutoff,
    #[id = "amp"]
    Amp,
    #[id = "pw"]
    #[name = "Pulse Width"]
    PulseWidth,
    #[id = "fmidx"]
    #[name = "FM Index"]
    FmIndex,
    #[id = "blend"]
    #[name = "Add Blend"]
    Blend,
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDivision {
    #[id = "4-1"]
    #[name = "4/1"]
    D4_1,
    #[id = "2-1"]
    #[name = "2/1"]
    D2_1,
    #[id = "1-1"]
    #[name = "1/1"]
    D1_1,
    #[id = "1-2"]
    #[name = "1/2"]
    D1_2,
    #[id = "1-4"]
    #[name = "1/4"]
    D1_4,
    #[id = "1-8"]
    #[name = "1/8"]
    D1_8,
    #[id = "1-16"]
    #[name = "1/16"]
    D1_16,
    #[id = "1-32"]
    #[name = "1/32"]
    D1_32,
}

impl SyncDivision {
    /// Length of one LFO cycle in beats (quarter notes).
    pub fn beats(self) -> f32 {
        match self {
            SyncDivision::D4_1 => 16.0,
            SyncDivision::D2_1 => 8.0,
            SyncDivision::D1_1 => 4.0,
            SyncDivision::D1_2 => 2.0,
            SyncDivision::D1_4 => 1.0,
            SyncDivision::D1_8 => 0.5,
            SyncDivision::D1_16 => 0.25,
            SyncDivision::D1_32 => 0.125,
        }
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceCount {
    #[id = "8"]
    V8,
    #[id = "16"]
    V16,
    #[id = "24"]
    V24,
    #[id = "32"]
    V32,
}

impl VoiceCount {
    pub fn count(self) -> usize {
        match self {
            VoiceCount::V8 => 8,
            VoiceCount::V16 => 16,
            VoiceCount::V24 => 24,
            VoiceCount::V32 => 32,
        }
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChorusMode {
    /// Wide 8 / 12.5 / 17 ms taps.
    #[id = "lush"]
    Lush,
    /// Tighter BBD-style 2.5 / 4 / 5.5 ms taps.
    #[id = "vint"]
    Vintage,
}

// ---------------------------------------------------------------------------
// Sub-structs
// ---------------------------------------------------------------------------

/// One harmonic drawbar. Nested as an array, so the IDs come out as
/// `<prefix>_amp_1` .. `<prefix>_amp_8`.
#[derive(Params)]
pub struct PartialParams {
    #[id = "amp"]
    pub amp: FloatParam,
}

impl PartialParams {
    fn new(index: usize) -> Self {
        // Default to a 1/k spectrum, which is a sawtooth-like starting point.
        let default = 1.0 / (index as f32 + 1.0);
        Self {
            amp: FloatParam::new(
                format!("Partial {}", index + 1),
                default,
                FloatRange::Linear { min: 0.0, max: 1.0 },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_percentage(1))
            .with_string_to_value(formatters::s2v_f32_percentage()),
        }
    }
}

#[derive(Params)]
pub struct OscParams {
    #[id = "wave"]
    pub wave: EnumParam<Waveform>,
    #[id = "coarse"]
    pub coarse: IntParam,
    #[id = "fine"]
    pub fine: FloatParam,
    #[id = "pw"]
    pub pulse_width: FloatParam,
    #[id = "level"]
    pub level: FloatParam,
    #[id = "blend"]
    pub blend: FloatParam,

    #[nested(array)]
    pub partials: [PartialParams; N_PARTIALS],

    #[id = "tilt"]
    pub tilt: FloatParam,
    #[id = "pdecay"]
    pub partial_decay: FloatParam,
    #[id = "oddeven"]
    pub odd_even: FloatParam,
}

impl OscParams {
    fn new(default_level_db: f32, default_coarse: i32) -> Self {
        Self {
            wave: EnumParam::new("Waveform", Waveform::Saw),
            coarse: IntParam::new(
                "Coarse",
                default_coarse,
                IntRange::Linear { min: -24, max: 24 },
            )
            .with_unit(" st"),
            fine: FloatParam::new(
                "Fine",
                0.0,
                FloatRange::Linear {
                    min: -100.0,
                    max: 100.0,
                },
            )
            .with_unit(" ct")
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_rounded(1)),
            pulse_width: FloatParam::new(
                "Pulse Width",
                0.5,
                FloatRange::Linear { min: 0.02, max: 0.98 },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_percentage(1))
            .with_string_to_value(formatters::s2v_f32_percentage()),
            level: FloatParam::new(
                "Level",
                util::db_to_gain(default_level_db),
                FloatRange::Skewed {
                    min: util::db_to_gain(-60.0),
                    max: util::db_to_gain(0.0),
                    factor: FloatRange::gain_skew_factor(-60.0, 0.0),
                },
            )
            .with_smoother(SmoothingStyle::Logarithmic(20.0))
            .with_unit(" dB")
            .with_value_to_string(formatters::v2s_f32_gain_to_db(1))
            .with_string_to_value(formatters::s2v_f32_gain_to_db()),
            blend: FloatParam::new("Additive Blend", 0.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(20.0))
                .with_value_to_string(formatters::v2s_f32_percentage(1))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            partials: std::array::from_fn(PartialParams::new),
            tilt: FloatParam::new("Tilt", 0.0, FloatRange::Linear { min: -1.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(30.0))
                .with_value_to_string(formatters::v2s_f32_rounded(2)),
            // Amplitude of partial 8 relative to partial 1, in dB.
            partial_decay: FloatParam::new(
                "Partial Decay",
                -12.0,
                FloatRange::Linear { min: -60.0, max: 0.0 },
            )
            .with_unit(" dB")
            .with_smoother(SmoothingStyle::Linear(30.0))
            .with_value_to_string(formatters::v2s_f32_rounded(1)),
            odd_even: FloatParam::new("Odd / Even", 0.0, FloatRange::Linear { min: -1.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(30.0))
                .with_value_to_string(formatters::v2s_f32_rounded(2)),
        }
    }
}

#[derive(Params)]
pub struct FmParams {
    #[id = "mode"]
    pub mode: EnumParam<FmRatioMode>,
    #[id = "ratio"]
    pub ratio: FloatParam,
    #[id = "rfine"]
    pub ratio_fine: FloatParam,
    #[id = "fixhz"]
    pub fixed_hz: FloatParam,
    #[id = "idx1"]
    pub index1: FloatParam,
    #[id = "idx2"]
    pub index2: FloatParam,
    #[id = "fb"]
    pub feedback: FloatParam,
    #[id = "envamt"]
    pub env_amount: FloatParam,
    #[id = "ktrk"]
    pub keytrack: FloatParam,
}

impl Default for FmParams {
    fn default() -> Self {
        Self {
            mode: EnumParam::new("FM Mode", FmRatioMode::Harmonic),
            ratio: FloatParam::new(
                "FM Ratio",
                2.0,
                FloatRange::Skewed {
                    min: 0.1,
                    max: 16.0,
                    factor: FloatRange::skew_factor(-1.0),
                },
            )
            .with_smoother(SmoothingStyle::Logarithmic(20.0))
            .with_value_to_string(formatters::v2s_f32_rounded(3)),
            ratio_fine: FloatParam::new(
                "FM Ratio Fine",
                0.0,
                FloatRange::Linear { min: -100.0, max: 100.0 },
            )
            .with_unit(" ct")
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_rounded(1)),
            fixed_hz: FloatParam::new(
                "FM Fixed Freq",
                440.0,
                FloatRange::Skewed {
                    min: 1.0,
                    max: 10_000.0,
                    factor: FloatRange::skew_factor(-2.0),
                },
            )
            .with_smoother(SmoothingStyle::Logarithmic(20.0))
            .with_value_to_string(formatters::v2s_f32_hz_then_khz(1))
            .with_string_to_value(formatters::s2v_f32_hz_then_khz()),
            index1: FloatParam::new(
                "FM -> Osc 1",
                0.0,
                FloatRange::Skewed {
                    min: 0.0,
                    max: 4.0,
                    factor: FloatRange::skew_factor(-1.0),
                },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_rounded(3)),
            index2: FloatParam::new(
                "FM -> Osc 2",
                0.0,
                FloatRange::Skewed {
                    min: 0.0,
                    max: 4.0,
                    factor: FloatRange::skew_factor(-1.0),
                },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_rounded(3)),
            feedback: FloatParam::new("FM Feedback", 0.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(20.0))
                .with_value_to_string(formatters::v2s_f32_percentage(1))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            // Bipolar: positive fades the index in from the envelope, negative inverts it.
            env_amount: FloatParam::new(
                "FM Env Amount",
                0.0,
                FloatRange::Linear { min: -1.0, max: 1.0 },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_percentage(1))
            .with_string_to_value(formatters::s2v_f32_percentage()),
            keytrack: FloatParam::new(
                "FM Index Keytrack",
                0.25,
                FloatRange::Linear { min: 0.0, max: 1.0 },
            )
            .with_value_to_string(formatters::v2s_f32_percentage(1))
            .with_string_to_value(formatters::s2v_f32_percentage()),
        }
    }
}

#[derive(Params)]
pub struct FilterParams {
    #[id = "mode"]
    pub mode: EnumParam<FilterMode>,
    #[id = "cutoff"]
    pub cutoff: FloatParam,
    #[id = "reso"]
    pub resonance: FloatParam,
    #[id = "drive"]
    pub drive: FloatParam,
    #[id = "ktrk"]
    pub keytrack: FloatParam,
    #[id = "envamt"]
    pub env_amount: FloatParam,
    #[id = "velamt"]
    pub vel_amount: FloatParam,
    #[id = "rcomp"]
    pub reso_comp: FloatParam,
}

impl Default for FilterParams {
    fn default() -> Self {
        Self {
            mode: EnumParam::new("Filter Mode", FilterMode::Lp24),
            cutoff: FloatParam::new(
                "Cutoff",
                12_000.0,
                FloatRange::Skewed {
                    min: 20.0,
                    max: 20_000.0,
                    factor: FloatRange::skew_factor(-2.0),
                },
            )
            // Logarithmic smoothing is exponential in Hz, i.e. linear in pitch,
            // which is the perceptually correct way to glide a cutoff.
            .with_smoother(SmoothingStyle::Logarithmic(20.0))
            .with_value_to_string(formatters::v2s_f32_hz_then_khz(1))
            .with_string_to_value(formatters::s2v_f32_hz_then_khz()),
            resonance: FloatParam::new("Resonance", 0.1, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(20.0))
                .with_value_to_string(formatters::v2s_f32_percentage(1))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            drive: FloatParam::new(
                "Drive",
                0.0,
                FloatRange::Linear { min: 0.0, max: 24.0 },
            )
            .with_unit(" dB")
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_rounded(1)),
            keytrack: FloatParam::new("Keytrack", 0.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            env_amount: FloatParam::new(
                "Env Amount",
                0.0,
                FloatRange::Linear { min: -64.0, max: 64.0 },
            )
            .with_unit(" st")
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_rounded(1)),
            vel_amount: FloatParam::new(
                "Velocity -> Cutoff",
                0.0,
                FloatRange::Linear { min: -48.0, max: 48.0 },
            )
            .with_unit(" st")
            .with_value_to_string(formatters::v2s_f32_rounded(1)),
            reso_comp: FloatParam::new(
                "Reso Compensation",
                0.6,
                FloatRange::Linear { min: 0.0, max: 1.0 },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_percentage(0))
            .with_string_to_value(formatters::s2v_f32_percentage()),
        }
    }
}

#[derive(Params)]
pub struct AdsrParams {
    #[id = "a"]
    pub attack: FloatParam,
    #[id = "d"]
    pub decay: FloatParam,
    #[id = "s"]
    pub sustain: FloatParam,
    #[id = "r"]
    pub release: FloatParam,
    #[id = "curve"]
    pub curve: FloatParam,
}

impl AdsrParams {
    fn new(name: &str, a: f32, d: f32, s: f32, r: f32) -> Self {
        let ms = |label: &str, default: f32, min: f32| {
            FloatParam::new(
                format!("{name} {label}"),
                default,
                FloatRange::Skewed {
                    min,
                    max: 20_000.0,
                    factor: FloatRange::skew_factor(-2.5),
                },
            )
            .with_unit(" ms")
            .with_smoother(SmoothingStyle::Logarithmic(20.0))
            .with_value_to_string(formatters::v2s_f32_rounded(1))
        };
        Self {
            attack: ms("Attack", a, 0.5),
            decay: ms("Decay", d, 1.0),
            sustain: FloatParam::new(
                format!("{name} Sustain"),
                s,
                FloatRange::Linear { min: 0.0, max: 1.0 },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_percentage(1))
            .with_string_to_value(formatters::s2v_f32_percentage()),
            release: ms("Release", r, 1.0),
            curve: FloatParam::new(
                format!("{name} Curve"),
                0.5,
                FloatRange::Linear { min: 0.0, max: 1.0 },
            )
            .with_value_to_string(formatters::v2s_f32_percentage(0))
            .with_string_to_value(formatters::s2v_f32_percentage()),
        }
    }
}

#[derive(Params)]
pub struct VoiceParams {
    #[id = "unison"]
    pub unison: IntParam,
    #[id = "detune"]
    pub detune: FloatParam,
    #[id = "spread"]
    pub spread: FloatParam,
    #[id = "analog"]
    pub analog: FloatParam,
    #[id = "count"]
    pub voice_count: EnumParam<VoiceCount>,
    #[id = "bend"]
    pub bend_range: IntParam,
    #[id = "prand"]
    pub phase_random: FloatParam,
    #[id = "vibrato"]
    pub vibrato: FloatParam,
}

impl Default for VoiceParams {
    fn default() -> Self {
        Self {
            unison: IntParam::new("Unison", 1, IntRange::Linear { min: 1, max: 7 })
                .with_unit(" v"),
            detune: FloatParam::new("Detune", 12.0, FloatRange::Linear { min: 0.0, max: 50.0 })
                .with_unit(" ct")
                .with_smoother(SmoothingStyle::Linear(30.0))
                .with_value_to_string(formatters::v2s_f32_rounded(1)),
            spread: FloatParam::new("Spread", 0.6, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(30.0))
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            analog: FloatParam::new("Analog", 0.25, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(50.0))
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            voice_count: EnumParam::new("Voices", VoiceCount::V16),
            bend_range: IntParam::new("Bend Range", 2, IntRange::Linear { min: 0, max: 24 })
                .with_unit(" st"),
            phase_random: FloatParam::new(
                "Phase Random",
                1.0,
                FloatRange::Linear { min: 0.0, max: 1.0 },
            )
            .with_value_to_string(formatters::v2s_f32_percentage(0))
            .with_string_to_value(formatters::s2v_f32_percentage()),
            vibrato: FloatParam::new("Vibrato", 0.0, FloatRange::Linear { min: 0.0, max: 100.0 })
                .with_unit(" ct")
                .with_smoother(SmoothingStyle::Linear(30.0))
                .with_value_to_string(formatters::v2s_f32_rounded(1)),
        }
    }
}

#[derive(Params)]
pub struct LfoParams {
    #[id = "shape"]
    pub shape: EnumParam<LfoShape>,
    #[id = "rate"]
    pub rate: FloatParam,
    #[id = "sync"]
    pub sync: BoolParam,
    #[id = "div"]
    pub division: EnumParam<SyncDivision>,
    #[id = "depth"]
    pub depth: FloatParam,
    #[id = "dest"]
    pub dest: EnumParam<LfoDest>,
    #[id = "delay"]
    pub delay: FloatParam,
    #[id = "fade"]
    pub fade: FloatParam,
}

impl LfoParams {
    fn new(name: &str, default_rate: f32, default_dest: LfoDest) -> Self {
        Self {
            shape: EnumParam::new(format!("{name} Shape"), LfoShape::Triangle),
            rate: FloatParam::new(
                format!("{name} Rate"),
                default_rate,
                FloatRange::Skewed {
                    min: 0.01,
                    max: 40.0,
                    factor: FloatRange::skew_factor(-1.5),
                },
            )
            .with_unit(" Hz")
            .with_smoother(SmoothingStyle::Logarithmic(20.0))
            .with_value_to_string(formatters::v2s_f32_rounded(2)),
            sync: BoolParam::new(format!("{name} Sync"), false),
            division: EnumParam::new(format!("{name} Division"), SyncDivision::D1_4),
            depth: FloatParam::new(
                format!("{name} Depth"),
                0.0,
                FloatRange::Linear { min: 0.0, max: 1.0 },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_percentage(0))
            .with_string_to_value(formatters::s2v_f32_percentage()),
            dest: EnumParam::new(format!("{name} Destination"), default_dest),
            delay: FloatParam::new(
                format!("{name} Delay"),
                0.0,
                FloatRange::Skewed {
                    min: 0.0,
                    max: 2000.0,
                    factor: FloatRange::skew_factor(-1.5),
                },
            )
            .with_unit(" ms")
            .with_value_to_string(formatters::v2s_f32_rounded(0)),
            fade: FloatParam::new(
                format!("{name} Fade In"),
                0.0,
                FloatRange::Skewed {
                    min: 0.0,
                    max: 2000.0,
                    factor: FloatRange::skew_factor(-1.5),
                },
            )
            .with_unit(" ms")
            .with_value_to_string(formatters::v2s_f32_rounded(0)),
        }
    }
}

#[derive(Params)]
pub struct ChorusParams {
    #[id = "mode"]
    pub mode: EnumParam<ChorusMode>,
    #[id = "rate"]
    pub rate: FloatParam,
    #[id = "depth"]
    pub depth: FloatParam,
    #[id = "fb"]
    pub feedback: FloatParam,
    #[id = "mix"]
    pub mix: FloatParam,
}

impl Default for ChorusParams {
    fn default() -> Self {
        Self {
            mode: EnumParam::new("Chorus Mode", ChorusMode::Lush),
            rate: FloatParam::new(
                "Chorus Rate",
                0.35,
                FloatRange::Skewed {
                    min: 0.05,
                    max: 5.0,
                    factor: FloatRange::skew_factor(-1.0),
                },
            )
            .with_unit(" Hz")
            .with_smoother(SmoothingStyle::Logarithmic(50.0))
            .with_value_to_string(formatters::v2s_f32_rounded(2)),
            depth: FloatParam::new("Chorus Depth", 0.6, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(50.0))
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            feedback: FloatParam::new(
                "Chorus Feedback",
                0.0,
                FloatRange::Linear { min: 0.0, max: 0.5 },
            )
            .with_smoother(SmoothingStyle::Linear(50.0))
            .with_value_to_string(formatters::v2s_f32_percentage(0))
            .with_string_to_value(formatters::s2v_f32_percentage()),
            mix: FloatParam::new("Chorus Mix", 0.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(50.0))
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
        }
    }
}

#[derive(Params)]
pub struct MasterParams {
    #[id = "gain"]
    pub gain: FloatParam,
    #[id = "tune"]
    pub tune: FloatParam,
    #[id = "noise"]
    pub noise: FloatParam,
}

impl Default for MasterParams {
    fn default() -> Self {
        Self {
            gain: FloatParam::new(
                "Master Gain",
                util::db_to_gain(-6.0),
                FloatRange::Skewed {
                    min: util::db_to_gain(-60.0),
                    max: util::db_to_gain(12.0),
                    factor: FloatRange::gain_skew_factor(-60.0, 12.0),
                },
            )
            .with_smoother(SmoothingStyle::Logarithmic(20.0))
            .with_unit(" dB")
            .with_value_to_string(formatters::v2s_f32_gain_to_db(1))
            .with_string_to_value(formatters::s2v_f32_gain_to_db()),
            tune: FloatParam::new(
                "Master Tune",
                0.0,
                FloatRange::Linear { min: -100.0, max: 100.0 },
            )
            .with_unit(" ct")
            .with_smoother(SmoothingStyle::Linear(30.0))
            .with_value_to_string(formatters::v2s_f32_rounded(1)),
            noise: FloatParam::new(
                "Noise Level",
                0.0,
                FloatRange::Skewed {
                    min: 0.0,
                    max: 1.0,
                    factor: FloatRange::skew_factor(-2.0),
                },
            )
            .with_smoother(SmoothingStyle::Linear(20.0))
            .with_value_to_string(formatters::v2s_f32_percentage(1))
            .with_string_to_value(formatters::s2v_f32_percentage()),
        }
    }
}

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

#[derive(Params)]
pub struct SynthParams {
    #[persist = "editor-state"]
    pub editor_state: Arc<EguiState>,
    /// Selected GUI colour scheme, an index into `editor::theme::THEMES`.
    /// Persisted with the project but not automatable.
    #[persist = "theme"]
    pub theme: Arc<AtomicU8>,

    #[nested(id_prefix = "o1", group = "Osc 1")]
    pub osc1: OscParams,
    #[nested(id_prefix = "o2", group = "Osc 2")]
    pub osc2: OscParams,

    #[nested(id_prefix = "fm", group = "FM")]
    pub fm: FmParams,

    #[nested(id_prefix = "flt", group = "Filter")]
    pub filter: FilterParams,

    #[nested(id_prefix = "ae", group = "Amp Env")]
    pub amp_env: AdsrParams,
    #[nested(id_prefix = "fe", group = "Filter Env")]
    pub filter_env: AdsrParams,
    #[nested(id_prefix = "me", group = "FM Env")]
    pub fm_env: AdsrParams,

    #[nested(id_prefix = "vc", group = "Voice")]
    pub voice: VoiceParams,

    #[nested(id_prefix = "l1", group = "LFO 1")]
    pub lfo1: LfoParams,
    #[nested(id_prefix = "l2", group = "LFO 2")]
    pub lfo2: LfoParams,

    #[nested(id_prefix = "ch", group = "Chorus")]
    pub chorus: ChorusParams,

    #[nested(id_prefix = "ms", group = "Master")]
    pub master: MasterParams,
}

impl Default for SynthParams {
    fn default() -> Self {
        Self {
            editor_state: EguiState::from_size(crate::editor::WIDTH, crate::editor::HEIGHT),
            theme: Arc::new(AtomicU8::new(crate::editor::theme::DEFAULT_THEME)),
            osc1: OscParams::new(-6.0, 0),
            osc2: OscParams::new(-60.0, -12),
            fm: FmParams::default(),
            filter: FilterParams::default(),
            amp_env: AdsrParams::new("Amp", 4.0, 400.0, 0.7, 300.0),
            filter_env: AdsrParams::new("Filter", 4.0, 600.0, 0.3, 400.0),
            fm_env: AdsrParams::new("FM", 1.0, 300.0, 0.0, 200.0),
            voice: VoiceParams::default(),
            lfo1: LfoParams::new("LFO 1", 5.2, LfoDest::Off),
            lfo2: LfoParams::new("LFO 2", 0.4, LfoDest::Off),
            chorus: ChorusParams::default(),
            master: MasterParams::default(),
        }
    }
}
