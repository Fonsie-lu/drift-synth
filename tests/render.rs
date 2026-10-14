//! End-to-end voice rendering checks: does it make sound, is it clean, and
//! does voice stealing behave.

use synth::params::Waveform;
use synth::voice::{VoiceBlock, VoiceManager};
use synth::CTL_BLOCK;

const SR: f32 = 48_000.0;
/// Power of two so the FFT is radix-2 and so a bin-aligned tone is periodic
/// in the window, which removes leakage without needing a window function.
const FFT_N: usize = 16_384;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Render `samples` frames of whatever is currently sounding.
fn render(mgr: &mut VoiceManager, block: &VoiceBlock, samples: usize) -> (Vec<f32>, Vec<f32>) {
    let mut left = vec![0.0; samples];
    let mut right = vec![0.0; samples];
    let mut pos = 0;
    while pos < samples {
        let len = CTL_BLOCK.min(samples - pos);
        for v in mgr.voices.iter_mut() {
            if v.is_free() {
                continue;
            }
            v.prepare_block(block, len);
            v.render(block, &mut left[pos..pos + len], &mut right[pos..pos + len]);
        }
        mgr.collect_finished();
        pos += len;
    }
    (left, right)
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |a, s| a.max(s.abs()))
}

/// Iterative radix-2 FFT, in place. Test-only, so clarity beats speed.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    assert!(n.is_power_of_two());

    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2usize;
    while len <= n {
        let angle = -2.0 * std::f64::consts::PI / len as f64;
        let (wr, wi) = (angle.cos(), angle.sin());
        let mut i = 0usize;
        while i < n {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let (ur, ui) = (re[i + k], im[i + k]);
                let (vr, vi) = (
                    re[i + k + len / 2] * cr - im[i + k + len / 2] * ci,
                    re[i + k + len / 2] * ci + im[i + k + len / 2] * cr,
                );
                re[i + k] = ur + vr;
                im[i + k] = ui + vi;
                re[i + k + len / 2] = ur - vr;
                im[i + k + len / 2] = ui - vi;
                let next_cr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next_cr;
            }
            i += len;
        }
        len <<= 1;
    }
}

/// Four-term Blackman-Harris. Its -92 dB sidelobes are what let us measure
/// aliasing far below the level a Hann window could resolve; the price is an
/// eight-bin main lobe, so analysis has to skip that far around each peak.
const MAIN_LOBE: usize = 8;

fn spectrum(signal: &[f32]) -> Vec<f64> {
    const A: [f64; 4] = [0.35875, 0.48829, 0.14128, 0.01168];
    let mut re: Vec<f64> = signal
        .iter()
        .take(FFT_N)
        .enumerate()
        .map(|(n, &x)| {
            let t = std::f64::consts::TAU * n as f64 / FFT_N as f64;
            let w = A[0] - A[1] * t.cos() + A[2] * (2.0 * t).cos() - A[3] * (3.0 * t).cos();
            x as f64 * w
        })
        .collect();
    let mut im = vec![0.0; FFT_N];
    fft(&mut re, &mut im);
    (0..FFT_N / 2)
        .map(|i| (re[i] * re[i] + im[i] * im[i]).sqrt())
        .collect()
}

/// Render one steady note at `f0` and return its magnitude spectrum.
fn spectrum_of_note(f0: f32, configure: impl FnOnce(&mut VoiceBlock)) -> Vec<f64> {
    let mut mgr = VoiceManager::default();
    mgr.sample_rate = SR;
    let mut block = VoiceBlock::defaults(SR);
    block.amp_adsr = [0.5, 10.0, 1.0, 10.0, 0.5];
    configure(&mut block);

    // Drive the pitch through the cents offset so the tone can sit exactly on
    // an FFT bin rather than wherever the nearest MIDI note happens to fall.
    block.global_cents = 1200.0 * (f0 / 440.0).log2();
    mgr.note_on(Some(1), 0, 69, 1.0, 1, 0.0, 0.0);

    // Skip the attack, then analyse one window.
    let (l, _) = render(&mut mgr, &block, FFT_N * 2);
    spectrum(&l[FFT_N..])
}

/// Worst component that is not one of `harmonics`, relative to the first, in dB.
fn worst_spur_dbc(mag: &[f64], harmonic_bins: &[usize]) -> (f64, usize) {
    let fundamental = mag[harmonic_bins[0]];
    assert!(fundamental > 1.0, "no fundamental in the spectrum");

    let mut worst = 0.0f64;
    let mut worst_bin = 0usize;
    for (bin, &m) in mag.iter().enumerate().skip(MAIN_LOBE * 2) {
        if harmonic_bins.iter().any(|&h| bin.abs_diff(h) <= MAIN_LOBE) {
            continue;
        }
        if m > worst {
            worst = m;
            worst_bin = bin;
        }
    }
    (20.0 * (worst / fundamental).log10(), worst_bin)
}

// ---------------------------------------------------------------------------
// It makes sound
// ---------------------------------------------------------------------------

#[test]
fn a_note_produces_audio() {
    let mut mgr = VoiceManager::default();
    mgr.sample_rate = SR;
    let block = VoiceBlock::defaults(SR);

    mgr.note_on(Some(1), 0, 60, 1.0, 1, 0.0, 0.0);
    assert_eq!(mgr.active_count(), 1);

    let (l, r) = render(&mut mgr, &block, 4800);
    assert!(peak(&l) > 0.1, "left channel is silent, peak {}", peak(&l));
    assert!(peak(&r) > 0.1, "right channel is silent");
    assert!(l.iter().all(|s| s.is_finite()), "non-finite sample");
}

#[test]
fn silence_before_any_note() {
    let mut mgr = VoiceManager::default();
    mgr.sample_rate = SR;
    let block = VoiceBlock::defaults(SR);
    let (l, r) = render(&mut mgr, &block, 2048);
    assert_eq!(peak(&l), 0.0);
    assert_eq!(peak(&r), 0.0);
}

#[test]
fn note_off_releases_and_frees_the_voice() {
    let mut mgr = VoiceManager::default();
    mgr.sample_rate = SR;
    let mut block = VoiceBlock::defaults(SR);
    block.amp_adsr = [1.0, 10.0, 0.5, 20.0, 0.5];

    mgr.note_on(Some(1), 0, 60, 1.0, 1, 0.0, 0.0);
    render(&mut mgr, &block, 4800);
    assert_eq!(mgr.active_count(), 1);

    mgr.note_off(Some(1), 0, 60, false);
    render(&mut mgr, &block, 9600);
    assert_eq!(mgr.active_count(), 0, "voice never freed after release");
    assert_eq!(mgr.terminated.len(), 1, "host was not told the voice ended");
}

#[test]
fn choke_stops_the_voice_immediately() {
    let mut mgr = VoiceManager::default();
    mgr.sample_rate = SR;
    mgr.note_on(Some(1), 0, 60, 1.0, 3, 0.0, 0.0);
    assert_eq!(mgr.active_count(), 3);
    mgr.choke(Some(1), 0, 60);
    assert_eq!(mgr.active_count(), 0);
    assert_eq!(mgr.terminated.len(), 3);
}

// ---------------------------------------------------------------------------
// Aliasing
// ---------------------------------------------------------------------------

/// Aliasing of the PolyBLEP saw across the keyboard. Two-point PolyBLEP is a
/// deliberate cost/quality choice, so this records the actual curve rather
/// than asserting one flat number: the residual grows with pitch because the
/// correction kernel covers a larger fraction of a cycle.
#[test]
fn sawtooth_aliasing_stays_within_the_documented_envelope() {
    // (bin of the fundamental, worst tolerated spur in dBc)
    let cases: [(usize, f64); 4] = [
        (89, -60.0),  // ~261 Hz, C4
        (179, -52.0), // ~524 Hz, C5
        (357, -46.0), // ~1046 Hz, C6
        (715, -40.0), // ~2095 Hz, C7
    ];

    for (f0_bin, limit) in cases {
        let f0 = f0_bin as f32 * SR / FFT_N as f32;
        let mag = spectrum_of_note(f0, |b| b.wave = [Waveform::Saw; 2]);

        let harmonics: Vec<usize> = (1..)
            .map(|k| k * f0_bin)
            .take_while(|&b| b < FFT_N / 2)
            .collect();
        let (dbc, bin) = worst_spur_dbc(&mag, &harmonics);

        println!(
            "saw @ {:7.1} Hz: worst spur {:6.1} dBc at {:6.0} Hz",
            f0,
            dbc,
            bin as f32 * SR / FFT_N as f32
        );
        assert!(
            dbc < limit,
            "saw at {f0:.0} Hz aliased to {dbc:.1} dBc, limit {limit}"
        );
    }
}

#[test]
fn a_sine_oscillator_is_spectrally_clean() {
    const F0_BIN: usize = 431;
    let f0 = F0_BIN as f32 * SR / FFT_N as f32;
    // Low level, so the chain is linear and this measures the lookup table
    // rather than the filter's input saturator.
    let mag = spectrum_of_note(f0, |b| {
        b.wave = [Waveform::Sine; 2];
        b.level = [0.05, 0.0];
    });
    let (dbc, bin) = worst_spur_dbc(&mag, &[F0_BIN]);
    println!("sine @ {f0:.1} Hz: worst spur {dbc:.1} dBc");
    assert!(
        dbc < -80.0,
        "sine had {dbc:.1} dBc of junk at bin {bin}; the LUT should be far cleaner"
    );
}

#[test]
fn a_triangle_is_cleaner_than_a_saw_at_the_same_pitch() {
    const F0_BIN: usize = 357;
    let f0 = F0_BIN as f32 * SR / FFT_N as f32;
    let harmonics: Vec<usize> = (1..)
        .map(|k| k * F0_BIN)
        .take_while(|&b| b < FFT_N / 2)
        .collect();

    let tri = spectrum_of_note(f0, |b| b.wave = [Waveform::Triangle; 2]);
    let (tri_dbc, _) = worst_spur_dbc(&tri, &harmonics);
    println!("triangle @ {f0:.1} Hz: worst spur {tri_dbc:.1} dBc");
    // PolyBLAMP corrects a slope discontinuity rather than a step, so the
    // residual is an order smaller than the saw's.
    assert!(tri_dbc < -75.0, "triangle aliased to {tri_dbc:.1} dBc");
}

#[test]
fn a_pulse_is_band_limited_on_both_edges() {
    const F0_BIN: usize = 357;
    let f0 = F0_BIN as f32 * SR / FFT_N as f32;
    let harmonics: Vec<usize> = (1..)
        .map(|k| k * F0_BIN)
        .take_while(|&b| b < FFT_N / 2)
        .collect();

    // A narrow pulse has far more high-order harmonics than a square, so its
    // aliasing floor is legitimately higher.
    for (width, limit) in [(0.1_f32, -40.0), (0.3, -46.0), (0.5, -47.0)] {
        let mag = spectrum_of_note(f0, |b| {
            b.wave = [Waveform::Pulse; 2];
            b.pulse_width = [width; 2];
        });
        let (dbc, _) = worst_spur_dbc(&mag, &harmonics);
        println!("pulse w={width} @ {f0:.1} Hz: worst spur {dbc:.1} dBc");
        assert!(dbc < limit, "pulse at width {width} aliased to {dbc:.1} dBc");
    }
}

// ---------------------------------------------------------------------------
// Unison and stealing
// ---------------------------------------------------------------------------

#[test]
fn unison_claims_one_slot_per_sub_voice() {
    let mut mgr = VoiceManager::default();
    mgr.sample_rate = SR;
    mgr.capacity = 16;
    mgr.note_on(Some(1), 0, 60, 1.0, 7, 0.0, 1.0);
    assert_eq!(mgr.active_count(), 7);
    mgr.note_on(Some(2), 0, 64, 1.0, 7, 0.0, 1.0);
    assert_eq!(mgr.active_count(), 14);
}

#[test]
fn stealing_takes_whole_groups_never_part_of_one() {
    let mut mgr = VoiceManager::default();
    mgr.sample_rate = SR;
    mgr.capacity = 16;
    let block = VoiceBlock::defaults(SR);

    // Fill the pool with two 7-voice stacks, letting each settle so the
    // "allocated this block" guard does not protect them.
    for (i, note) in [60u8, 64].into_iter().enumerate() {
        mgr.block_stamp += 1;
        mgr.note_on(Some(i as i32 + 1), 0, note, 1.0, 7, 0.0, 1.0);
        render(&mut mgr, &block, 480);
    }
    assert_eq!(mgr.active_count(), 14);

    // A third stack must displace a whole group.
    mgr.block_stamp += 1;
    mgr.note_on(Some(3), 0, 67, 1.0, 7, 0.0, 1.0);

    // Every group that still has voices must have all of them, or all of them
    // fading together. Count per note_id and per state.
    let mut groups: std::collections::HashMap<u32, (usize, usize)> = Default::default();
    for v in mgr.voices.iter().filter(|v| !v.is_free()) {
        let e = groups.entry(v.note_id).or_default();
        e.0 += 1;
        if v.state == synth::voice::VoiceState::Stealing {
            e.1 += 1;
        }
    }
    for (id, (total, stealing)) in groups {
        assert_eq!(total, 7, "group {id} was split, only {total} voices left");
        assert!(
            stealing == 0 || stealing == total,
            "group {id} is half-stolen: {stealing} of {total}"
        );
    }
}

#[test]
fn a_stolen_voice_fades_instead_of_cutting() {
    // Sine voices, so any step in the output comes from voice management
    // rather than from a sawtooth's own discontinuity.
    let scenario = |hard_cut: bool| {
        let mut mgr = VoiceManager::default();
        mgr.sample_rate = SR;
        mgr.capacity = 1;
        let mut block = VoiceBlock::defaults(SR);
        block.wave = [Waveform::Sine; 2];
        block.amp_adsr = [0.5, 5000.0, 1.0, 5000.0, 0.5];

        mgr.note_on(Some(1), 0, 60, 1.0, 1, 0.0, 0.0);
        let (mut l, _) = render(&mut mgr, &block, 4800);

        mgr.block_stamp += 1;
        if hard_cut {
            mgr.choke(Some(1), 0, 60);
        }
        mgr.note_on(Some(2), 0, 67, 1.0, 1, 0.0, 0.0);

        // Append rather than restart, so the discontinuity at the splice is
        // inside the window we measure.
        let (tail, _) = render(&mut mgr, &block, 960);
        l.extend_from_slice(&tail);
        l.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max)
    };

    let faded = scenario(false);
    let cut = scenario(true);
    println!("max step: faded {faded:.4}, hard cut {cut:.4}");

    assert!(cut > 0.2, "the hard-cut control did not actually click ({cut})");
    assert!(
        faded < cut * 0.25,
        "stealing stepped by {faded}, barely better than the {cut} hard cut"
    );
}

#[test]
fn the_declick_reserve_lets_a_replacement_start_immediately() {
    let mut mgr = VoiceManager::default();
    mgr.sample_rate = SR;
    mgr.capacity = 8;
    let block = VoiceBlock::defaults(SR);

    for note in 60u8..68 {
        mgr.block_stamp += 1;
        mgr.note_on(Some(note as i32), 0, note, 1.0, 1, 0.0, 0.0);
    }
    render(&mut mgr, &block, 480);

    mgr.block_stamp += 1;
    mgr.note_on(Some(99), 0, 72, 1.0, 1, 0.0, 0.0);
    let started = mgr
        .voices
        .iter()
        .any(|v| !v.is_free() && v.voice_id == 99 && v.state == synth::voice::VoiceState::Active);
    assert!(started, "the new note had to wait for a slot");
}

// ---------------------------------------------------------------------------
// Analog character
// ---------------------------------------------------------------------------

#[test]
fn analog_zero_makes_repeated_notes_identical() {
    let block = VoiceBlock::defaults(SR);
    let run = || {
        let mut mgr = VoiceManager::default();
        mgr.sample_rate = SR;
        mgr.note_on(Some(1), 0, 60, 1.0, 1, 0.0, 0.0);
        let (a, _) = render(&mut mgr, &block, 2048);
        mgr.choke(Some(1), 0, 60);
        mgr.note_on(Some(2), 0, 60, 1.0, 1, 0.0, 0.0);
        let (b, _) = render(&mut mgr, &block, 2048);
        (a, b)
    };
    let (first, second) = run();
    // Not bit-identical: the tiny always-on noise seed is keyed to the note's
    // id so the filter can start self-oscillating. Everything else must match.
    let diff = first
        .iter()
        .zip(&second)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        diff < 1e-4,
        "analog = 0 still varied between two identical notes by {diff}"
    );
}

#[test]
fn analog_one_makes_repeated_notes_differ() {
    let mut block = VoiceBlock::defaults(SR);
    block.analog = 1.0;

    let mut mgr = VoiceManager::default();
    mgr.sample_rate = SR;
    mgr.note_on(Some(1), 0, 60, 1.0, 1, 1.0, 0.0);
    let (first, _) = render(&mut mgr, &block, 4096);
    mgr.choke(Some(1), 0, 60);
    mgr.note_on(Some(2), 0, 60, 1.0, 1, 1.0, 0.0);
    let (second, _) = render(&mut mgr, &block, 4096);

    let diff: f32 = first
        .iter()
        .zip(&second)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(
        diff > 0.01,
        "analog = 1 barely varied between two notes (diff {diff})"
    );
}

#[test]
fn analog_character_stays_bounded() {
    let mut block = VoiceBlock::defaults(SR);
    block.analog = 1.0;
    block.resonance = 1.0;
    block.drive_gain = 16.0;
    block.cutoff_hz = 800.0;

    let mut mgr = VoiceManager::default();
    mgr.sample_rate = SR;
    mgr.note_on(Some(1), 0, 36, 1.0, 7, 1.0, 1.0);
    let (l, r) = render(&mut mgr, &block, (SR as usize) * 2);

    assert!(l.iter().all(|s| s.is_finite()), "non-finite left sample");
    assert!(r.iter().all(|s| s.is_finite()), "non-finite right sample");
    // The voice output is pre-limiter, so allow headroom, but it must not run away.
    assert!(peak(&l) < 20.0, "left peaked at {}", peak(&l));
}

// ---------------------------------------------------------------------------
// Additive engine
// ---------------------------------------------------------------------------

#[test]
fn additive_mode_produces_the_requested_harmonics() {
    const F0_BIN: usize = 256;
    let f0 = F0_BIN as f32 * SR / FFT_N as f32;

    let mut mgr = VoiceManager::default();
    mgr.sample_rate = SR;
    let mut block = VoiceBlock::defaults(SR);
    block.blend = [1.0, 1.0];
    block.amp_adsr = [0.5, 10.0, 1.0, 10.0, 0.5];
    // Fundamental and third harmonic only.
    let mut sliders = [0.0; synth::N_PARTIALS];
    sliders[0] = 1.0;
    sliders[2] = 1.0;
    block.sliders = [sliders; 2];
    block.tilt = [0.0; 2];
    block.partial_decay_db = [0.0; 2];

    let note = 69.0 + 12.0 * (f0 / 440.0).log2();
    block.global_cents = (note - note.round()) * 100.0;
    mgr.note_on(Some(1), 0, note.round() as u8, 1.0, 1, 0.0, 0.0);

    let (l, _) = render(&mut mgr, &block, FFT_N * 2);
    let mag = spectrum(&l[FFT_N..]);

    let h1 = mag[F0_BIN];
    let h2 = mag[F0_BIN * 2];
    let h3 = mag[F0_BIN * 3];
    assert!(h1 > 1.0, "no fundamental");
    assert!(h3 > 0.5 * h1, "third harmonic missing: {h3} vs {h1}");
    assert!(
        h2 < 0.01 * h1,
        "second harmonic should be absent, got {h2} vs {h1}"
    );
}
