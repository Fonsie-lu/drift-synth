//! Standalone build: `cargo run --release --features standalone --bin drift`

use nih_plug::prelude::nih_export_standalone;
use synth::Drift;

fn main() {
    nih_export_standalone::<Drift>();
}
