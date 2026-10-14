# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Drift is a polyphonic hybrid additive/subtractive synth plugin (CLAP + VST3) built on nih-plug, with a custom-painted nih_plug_egui editor (egui 0.31). Vendor is Fonsie Audio; the GUI follows the sibling project `~/tools/juno-clone`. The crate is named `synth`; the plugin and bundle are named `Drift`.

## Commands

```sh
cargo test                                   # all tests (tests/*.rs plus headless GUI tests in src/editor_tests.rs)
cargo test --lib                             # headless GUI tests only
cargo test --test render                     # one test file
cargo test --test dsp ladder_is_stable       # one test by name filter
cargo test --test render -- --nocapture      # prints the aliasing measurements quoted in README
cargo clippy --all-targets

# Plugin bundle -> target/bundled/Drift.{clap,vst3}
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo xtask bundle synth --release

# Standalone app (JACK/PipeWire or ALSA), fastest way to hear a change
cargo run --release --features standalone --bin drift -- --backend alsa

# Offline WAV render, no host needed; patches: pad, saw, additive, fm, bass, sweep
cargo run --release --example render -- out.wav pad

# Editor in a plain window, no host, no audio; DRIFT_THEME=0..3 picks the start theme
cargo run --release --example gui_preview
```

JACK and ALSA dev headers are not installed on this machine, so the `standalone` feature does not build here; use `gui_preview` for the GUI and `render` for audio. Screenshot workflow: start the preview with `timeout 6 ... &`, then `import -window "Drift Preview" out.png`, then Read the PNG. The window manager resizes the window to 1506x1369; the panel is scaled to the width and centred vertically, so crop with `magick out.png -crop 1506x918+0+225 +repage crop.png`.

`cargo xtask` is an alias in `.cargo/config.toml` for the `xtask` workspace member (nih_plug_xtask). nih-plug is pinned to a git rev; keep `nih_plug`, `nih_plug_egui` and `nih_plug_xtask` on the same rev. The `baseview` and `egui-baseview` dev-dependencies must match the revs nih_plug_egui uses.

Debug builds enable nih-plug's `assert_process_allocs`: any heap allocation inside `process()` aborts. Run debug builds when changing DSP or voice code.

## Architecture

**Audio path (`src/lib.rs`).** `process()` splits each host buffer into control blocks of at most `CTL_BLOCK` (32) samples. A block also ends at the next note event, so note-ons are sample-accurate. For each block:
1. `resolve_block()` reads every smoothed parameter once into a `VoiceBlock` (`src/voice.rs`). Voices never read `SynthParams` directly; they read only `VoiceBlock`.
2. Each active `Voice` calls `prepare_block()` (control-rate updates) and then `render()`, which sums into the stereo buffers.
3. `render_output()` runs the global chain: DC blocker, chorus, analog HF tilt, master gain, soft limiter (`src/fx.rs`).
4. If the editor is open, `update_visuals()` publishes state for the GUI.
5. Terminated voice IDs go back to the host as `VoiceTerminated` events.

When you add a parameter that voices use, add it in three places: `params.rs`, a field on `VoiceBlock`, and `resolve_block()`. Also update `VoiceBlock`'s neutral default, which the tests and `examples/render.rs` use.

**Voices (`src/voice.rs`).** `VoiceManager` owns a fixed pool of `VOICE_SLOTS` = `MAX_VOICES` (32) + `DECLICK_VOICES` (4). The user-selected voice count (8/16/24/32) limits only the normal slots. The declick reserve lets a stolen voice fade out (`STEAL_FADE_S`) while its replacement starts at once. A note uses one slot per unison sub-voice. Stealing always takes whole unison groups, never part of a group. Note-off and choke match on voice ID first, then fall back to channel + note.

**Per-voice chain.** Two oscillators (`osc.rs`) each blend a PolyBLEP/PolyBLAMP classic waveform with an 8-partial additive bank (`N_PARTIALS`). A third operator (`fm.rs`) phase-modulates them; it is PM, not true FM, by design (see module doc). Next is the ZDF/TPT ladder filter with a drive stage (`filter.rs`), then envelopes and LFOs (`env.rs`). LFO 1 runs per voice. LFO 2 is global and is resolved in `lib.rs`.

**Analog character (`src/analog.rs`).** Per-voice tolerances, drift, phase jitter and noise all scale with the single `ANALOG` macro. At `analog == 0` the output must be exactly clean and deterministic. `tests/dsp.rs` and `tests/render.rs` assert this.

**Sine LUT.** `build.rs` generates `SINE_LUT` into `OUT_DIR`. `osc.rs` pulls it in with `include!`. The table has one guard entry past the end so interpolation needs no bounds check.

**Audio-to-GUI (`src/visuals.rs`).** Scalars go through relaxed atomics in `VisualState`. The scope frame goes through a `triple_buffer`. The audio thread never locks or allocates here.

**Editor (`src/editor.rs`, `src/editor/theme.rs`).** Everything is painted on a fixed 1400x850 canvas (`BW`/`BH`) through `Cv` and scaled uniformly to the window; the default window is 110 % of the canvas. Four rows of sections, each with a coloured bar: OSC 1/2 + FM; ADDITIVE 1/2 + MONITOR (OLED, scope); FILTER + AMP/FILTER/FM ENV; LFO 1/2, VOICE, CHORUS, MASTER. `row(i)` gives the label baseline and fader slot of row `i`, `graph_rows(i)` the top and bottom of a captioned graph; x positions are hand-placed in `draw()` and the section functions. Controls: `fader`, `drawbar` (additive partials, with the post-macro level drawn behind the slot), `stepper` (enum and small int params, text or icon labels), the filter graph (`filter_graph`, cutoff on a log axis plus resonance) and envelope graphs (`env_graph` with `env_handle`s). Graphs compute their shapes with the DSP code itself (`osc::subtractive`/`additive`, `fm::ModOsc`, `filter::magnitude`, the `env::Adsr` RC segment formula), so they match the sound. The OLED is drawn last so it can read the focus set by any control in the same frame. Fonts are DejaVu, embedded from `assets/fonts/`.

- Faders accumulate per-frame drag deltas; graph handles use the offset from the press origin (`drag_offset`) because their spans are short and egui swallows the first pixels before a drag is recognised. Test drags on handles must exceed that threshold (about 6 px).
- Parameter values reach the plugin asynchronously. Read values only through `G::shown()` (which overlays pending values until the host applies them) and write only through `G::send()` inside a gesture or `G::change()`. Never compute a new value from the read-back value during a drag; drag position lives in egui temp memory.
- Display names repeat (both oscillators have "Fine", all envelopes have "Attack"), so pending values, the preview store and registered controls are keyed by `ParamPtr` or parameter ID, never by name. The ID map is rebuilt whenever the `SynthParams` address changes.
- Never hard-code colours in `editor.rs`. Add a field to `Theme` and fill it in for all four themes (Tokyo Night default, Catppuccin, Dracula, Gruvbox; same set as the juno clone). The theme index is persisted as `#[persist = "theme"]`, not an automatable parameter.
- `src/editor_tests.rs` runs the real editor in a headless egui context against a mock host that applies values late and checks gesture balance. Every control registers its rect in `EditorState::controls` (two-parameter handles and graphs under their horizontal parameter); `every_control_is_registered` asserts the counts and `every_used_parameter_has_a_control` lists the parameters without their own control. Update both when adding controls.
- No vendor name or logo on the panel.

## Constraints

- Parameter IDs (`#[id = ...]`) and ranges in `src/params.rs` are part of saved plugin state. Renaming or re-ranging a parameter breaks existing projects. `tests/params.rs` checks that IDs are unique and at most 32 characters.
- `CLAP_ID` (`ch.fonsie.drift`), `VST3_CLASS_ID` (`FonsieDriftSynth`) and enum variant IDs are also stable identifiers.
- LFO `sync`/`division` exist as parameters but the DSP ignores them, and LFO 2 ignores `delay`/`fade` because it is global. The editor hides these; wire them up in the DSP before adding controls.
- Test the CLAP build in Bitwig. On Linux, VST3 editors render black in Bitwig (nih-plug issue #271).
- License is GPL-3.0-or-later because of the VST3 export (`vst3-sys`).
