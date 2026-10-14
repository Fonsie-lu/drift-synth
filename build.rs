//! Generates the shared sine lookup table into OUT_DIR.
//!
//! The table gets one guard entry past the end so `SINE_LUT[i + 1]` is always
//! in bounds for `i <= SINE_LUT_SIZE - 1`, which lets LLVM drop the bounds
//! check in the interpolating lookup.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

const SIZE: usize = 2048;

fn main() {
    let mut src = String::with_capacity(SIZE * 16);
    writeln!(src, "pub const SINE_LUT_SIZE: usize = {SIZE};").unwrap();
    // The table naturally contains values like sin(pi/4), so clippy's
    // constant and precision lints would fire on hundreds of entries.
    writeln!(
        src,
        "#[allow(clippy::excessive_precision, clippy::approx_constant, clippy::unreadable_literal)]"
    )
    .unwrap();
    writeln!(src, "pub static SINE_LUT: [f32; {}] = [", SIZE + 1).unwrap();
    for i in 0..=SIZE {
        let phase = (i as f64) / (SIZE as f64) * std::f64::consts::TAU;
        // `{:?}` on f32 emits the shortest string that round-trips exactly.
        writeln!(src, "    {:?}_f32,", phase.sin() as f32).unwrap();
    }
    writeln!(src, "];").unwrap();

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    fs::write(Path::new(&out_dir).join("sine_lut.rs"), src).expect("failed to write sine LUT");

    println!("cargo:rerun-if-changed=build.rs");
}
