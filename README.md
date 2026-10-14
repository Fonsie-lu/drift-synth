# Drift

A polyphonic hybrid additive/subtractive synthesizer plugin for Linux, built with
[nih-plug](https://github.com/robbert-vdh/nih-plug). Exports **CLAP** and **VST3**.

- 16 voices (switchable 8/16/24/32), unison stacks of 1–7 sub-voices per note
- Two oscillators, each blending a band-limited classic waveform against an
  8-partial additive bank with TILT / DECAY / ODD-EVEN macros
- A third oscillator acting as an FM (phase modulation) operator with its own
  envelope, feedback, and automatic anti-alias index limiting
- Zero-delay-feedback transistor ladder filter: LP/HP/BP at 12 and 24 dB, notch,
  self-oscillating resonance, saturated drive
- Analog character: per-voice component tolerances, three-band pitch/filter/amp
  drift, oscillator phase jitter, pink noise and asymmetric saturation, all under
  one `ANALOG` macro
- Three-tap chorus with a rate-dependent depth limiter
- Hardware style panel with faders, live wave displays, draggable filter and
  envelope graphs, an OLED value display, oscilloscope, per-voice activity LEDs
  and four colour themes

## Building

```sh
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo xtask bundle synth --release
```

Artifacts land in `target/bundled/`. To build and install in one step:

```sh
./install.sh               # copy to ~/.clap and ~/.vst3
./install.sh --link        # symlink instead, so later rebuilds need no reinstall
./install.sh --system      # /usr/lib/clap and /usr/lib/vst3, via sudo
./install.sh --uninstall   # remove (combine with --system for the system folders)
```

`--clap` or `--vst3` limits the install to one format, and `--no-build` installs
the existing bundles. The build targets x86-64-v3 (AVX2); for older CPUs run
`TARGET_CPU=x86-64 ./install.sh`.

**Use the CLAP build.** There is an [open nih-plug
issue](https://github.com/robbert-vdh/nih-plug/issues/271) where VST3 editors
render black in Bitwig on Linux; CLAP is unaffected and is also the format that
gets polyphonic modulation and sample-accurate automation.

## Development

```sh
cargo test                 # DSP, parameter, rendering and headless GUI tests
cargo clippy --all-targets

# Editor in a plain window, no host and no audio
cargo run --release --example gui_preview

# Standalone app against JACK/PipeWire or ALSA - fastest way to hear a change
cargo run --release --features standalone --bin drift -- --backend alsa

# Offline render, for listening and for spectral analysis
cargo run --release --example render -- out.wav pad
# patches: pad, saw, additive, fm, bass, sweep
```

Build in debug while working on the DSP: `assert_process_allocs` is enabled, so
any allocation inside `process()` aborts immediately.

## Interface

The panel is laid out like a hardware fader programmer: coloured section bars,
faders with scales, and stepped faders with waveform icons for the switched
parameters. Oscillator shape, filter and envelopes are shown as graphs.

- Each oscillator has a wave display that follows waveform, pulse width,
  additive blend, partials and FM; its height follows the level fader.
- FILTER: drag the response graph sideways for cutoff and up/down for
  resonance. While notes sound, a second trace shows the cutoff actually
  reaching the newest voice.
- AMP, FILTER and FM ENV: drag the handles - attack time at the top, decay time
  and sustain level at the corner, release time at the end. A dot follows the
  playing amp and filter envelopes.
- Faders: drag (Shift for fine), mouse wheel, double-click resets to default.
  Graph handles work the same way.
- Stepped faders: drag, click a position or its label, or use the mouse wheel.
- The OLED shows the control under the mouse with its value; with nothing under
  the mouse it shows the voice count and the mixed oscillator wave. The scope
  next to it shows the output.
- THEME chips in the header switch between Tokyo Night (default), Catppuccin,
  Dracula and Gruvbox. The choice is saved with the project.
- Drag the bottom right corner to resize; the panel scales uniformly.

## Measured performance

Aliasing of the PolyBLEP sawtooth, worst non-harmonic component relative to the
fundamental (from `cargo test --test render -- --nocapture`):

| note | worst spur |
|------|-----------|
| C4 (261 Hz) | −62.9 dBc |
| C5 (524 Hz) | −53.5 dBc |
| C6 (1046 Hz) | −47.5 dBc |
| C7 (2095 Hz) | −41.5 dBc |

Triangle (PolyBLAMP) measures −78 dBc and the sine LUT −94 dBc at C6. Two-point
PolyBLEP is a deliberate cost/quality choice; the residual grows with pitch
because the correction kernel covers a larger fraction of each cycle. Moving to
a four-point kernel above roughly 500 Hz would buy about 20 dB in the top octave
if that ever matters.

CPU is roughly 350 cycles per sub-voice per sample, so 16 sounding sub-voices sit
near 6 % of one core at 48 kHz.

## Licence

GPL-3.0-or-later. This is forced by nih-plug's VST3 export, which links the
GPL'd `vst3-sys` bindings. Dropping the `vst3` default feature and shipping CLAP
only would leave the crate free to be licensed permissively.

Copyright (C) 2026 Beat Weber. Full text in [LICENSE](LICENSE).
