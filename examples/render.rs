//! Offline renderer: writes a WAV so patches can be listened to and analysed
//! without loading the plugin into a host.
//!
//!   cargo run --release --example render -- out.wav [patch]
//!
//! Patches: saw, additive, fm, pad, bass, sweep

use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};

use synth::params::{FmRatioMode, Waveform};
use synth::voice::{VoiceBlock, VoiceManager};
use synth::{filter, fx, CTL_BLOCK};

const SR: f32 = 48_000.0;

struct Note {
    start_s: f32,
    len_s: f32,
    key: u8,
}

fn main() {
    let mut args = env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "drift.wav".to_string());
    let patch = args.next().unwrap_or_else(|| "pad".to_string());

    let mut block = VoiceBlock::defaults(SR);
    let mut unison = 1usize;
    let mut analog = 0.25;
    let mut chorus_mix = 0.0;
    let notes: Vec<Note>;

    match patch.as_str() {
        "saw" => {
            block.wave = [Waveform::Saw; 2];
            block.cutoff_hz = 3000.0;
            block.resonance = 0.35;
            block.filter_env_amount = 36.0;
            block.filter_adsr = [2.0, 500.0, 0.2, 300.0, 0.5];
            block.amp_adsr = [3.0, 800.0, 0.7, 400.0, 0.5];
            notes = chord(&[60, 64, 67, 72], 0.0, 2.0);
        }
        "additive" => {
            block.blend = [1.0, 0.0];
            block.tilt = [-0.3; 2];
            block.partial_decay_db = [-9.0; 2];
            block.cutoff_hz = 18_000.0;
            block.amp_adsr = [5.0, 1500.0, 0.6, 600.0, 0.5];
            notes = chord(&[48, 55, 60, 64, 67], 0.0, 2.5);
        }
        "fm" => {
            block.wave = [Waveform::Sine; 2];
            block.fm_mode = FmRatioMode::Harmonic;
            block.fm_ratio = 3.0;
            block.fm_index = [1.2, 0.0];
            block.fm_env_amount = 1.0;
            block.fm_feedback = 0.3;
            block.fm_adsr = [1.0, 400.0, 0.05, 300.0, 0.7];
            block.amp_adsr = [1.0, 900.0, 0.25, 500.0, 0.6];
            block.cutoff_hz = 16_000.0;
            notes = (0..8)
                .map(|i| Note {
                    start_s: i as f32 * 0.35,
                    len_s: 0.30,
                    key: [60, 64, 67, 72, 71, 67, 64, 60][i],
                })
                .collect();
        }
        "bass" => {
            block.wave = [Waveform::Saw, Waveform::Pulse];
            block.level = [0.7, 0.45];
            block.coarse_cents = [0.0, -1200.0];
            block.cutoff_hz = 400.0;
            block.resonance = 0.55;
            block.drive_gain = 4.0;
            block.filter_env_amount = 40.0;
            block.filter_adsr = [1.0, 180.0, 0.1, 150.0, 0.7];
            block.amp_adsr = [1.0, 400.0, 0.8, 120.0, 0.5];
            analog = 0.5;
            notes = (0..12)
                .map(|i| Note {
                    start_s: i as f32 * 0.25,
                    len_s: 0.20,
                    key: [36, 36, 43, 36, 41, 36, 39, 36, 36, 43, 48, 43][i],
                })
                .collect();
        }
        "sweep" => {
            block.wave = [Waveform::Saw; 2];
            block.resonance = 0.85;
            block.amp_adsr = [10.0, 5000.0, 1.0, 500.0, 0.5];
            block.cutoff_hz = 100.0;
            notes = chord(&[40, 47, 52], 0.0, 4.0);
        }
        // "pad"
        _ => {
            block.wave = [Waveform::Saw; 2];
            block.level = [0.55, 0.4];
            block.coarse_cents = [0.0, 700.0];
            block.blend = [0.35, 0.0];
            block.tilt = [-0.2; 2];
            block.cutoff_hz = 2200.0;
            block.resonance = 0.3;
            block.filter_env_amount = 24.0;
            block.filter_adsr = [400.0, 2000.0, 0.4, 1500.0, 0.5];
            block.amp_adsr = [300.0, 2000.0, 0.8, 1200.0, 0.5];
            block.detune_cents = 18.0;
            block.spread = 0.8;
            unison = 5;
            analog = 0.6;
            chorus_mix = 0.45;
            notes = chord(&[48, 55, 60, 64], 0.0, 4.0);
        }
    }

    block.analog = analog;
    block.unison = unison;
    block.filter_taps = filter::mode_taps(block.filter_mode);

    let total_s = notes
        .iter()
        .map(|n| n.start_s + n.len_s)
        .fold(0.0f32, f32::max)
        + 3.0;
    let total = (total_s * SR) as usize;

    let mut mgr = VoiceManager::default();
    mgr.sample_rate = SR;
    mgr.capacity = 16;

    let mut chorus = fx::Chorus::default();
    chorus.set_sample_rate(SR);
    let mut dc_l = fx::DcBlocker::default();
    let mut dc_r = fx::DcBlocker::default();
    dc_l.set_sample_rate(SR);
    dc_r.set_sample_rate(SR);

    let mut left = vec![0.0f32; total];
    let mut right = vec![0.0f32; total];

    let mut pending_on: Vec<(usize, u8, i32)> = notes
        .iter()
        .enumerate()
        .map(|(i, n)| ((n.start_s * SR) as usize, n.key, i as i32 + 1))
        .collect();
    let mut pending_off: Vec<(usize, u8, i32)> = notes
        .iter()
        .enumerate()
        .map(|(i, n)| (((n.start_s + n.len_s) * SR) as usize, n.key, i as i32 + 1))
        .collect();
    pending_on.sort_by_key(|e| e.0);
    pending_off.sort_by_key(|e| e.0);

    let mut pos = 0usize;
    while pos < total {
        let len = CTL_BLOCK.min(total - pos);
        mgr.block_stamp += 1;

        while pending_on.first().is_some_and(|e| e.0 <= pos) {
            let (_, key, id) = pending_on.remove(0);
            mgr.note_on(Some(id), 0, key, 0.9, unison, analog, 1.0);
        }
        while pending_off.first().is_some_and(|e| e.0 <= pos) {
            let (_, key, id) = pending_off.remove(0);
            mgr.note_off(Some(id), 0, key, false);
        }

        for v in mgr.voices.iter_mut() {
            if v.is_free() {
                continue;
            }
            v.prepare_block(&block, len);
            v.render(&block, &mut left[pos..pos + len], &mut right[pos..pos + len]);
        }
        mgr.collect_finished();

        for i in pos..pos + len {
            let l = dc_l.next(left[i]);
            let r = dc_r.next(right[i]);
            let (cl, cr) = chorus.next(l, r, block.chorus_mode(), 0.35, 0.6, 0.0, chorus_mix);
            left[i] = fx::limit(cl * 0.5);
            right[i] = fx::limit(cr * 0.5);
        }
        pos += len;
    }

    let peak = left
        .iter()
        .chain(right.iter())
        .fold(0.0f32, |a, s| a.max(s.abs()));
    println!("patch '{patch}': {total_s:.1} s, peak {:.2} dBFS", 20.0 * peak.max(1e-9).log10());

    write_wav(&path, &left, &right).expect("failed to write wav");
    println!("wrote {path}");
}

fn chord(keys: &[u8], start_s: f32, len_s: f32) -> Vec<Note> {
    keys.iter()
        .map(|&key| Note {
            start_s,
            len_s,
            key,
        })
        .collect()
}

fn write_wav(path: &str, left: &[f32], right: &[f32]) -> std::io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    let frames = left.len().min(right.len());
    let data_bytes = (frames * 2 * 2) as u32;

    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_bytes).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&2u16.to_le_bytes())?; // stereo
    w.write_all(&(SR as u32).to_le_bytes())?;
    w.write_all(&((SR as u32) * 4).to_le_bytes())?;
    w.write_all(&4u16.to_le_bytes())?;
    w.write_all(&16u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_bytes.to_le_bytes())?;

    for i in 0..frames {
        for &s in &[left[i], right[i]] {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            w.write_all(&v.to_le_bytes())?;
        }
    }
    w.flush()
}
