use synth::analog::{Rng32, Tolerance};
use synth::env::{Adsr, EnvStage};
use synth::filter::{self, Ladder};
use synth::fm;
use synth::osc::{self, poly_blamp, poly_blep};
use synth::params::FilterMode;
use synth::N_PARTIALS;

const SR: f32 = 48_000.0;

// ---------------------------------------------------------------------------
// Band-limiting kernels
// ---------------------------------------------------------------------------

#[test]
fn poly_blep_is_zero_outside_the_kernel() {
    let dt = 0.01;
    for i in 0..100 {
        let t = 0.2 + 0.6 * i as f32 / 100.0;
        assert_eq!(poly_blep(t, dt), 0.0, "non-zero residual at t={t}");
    }
}

#[test]
fn poly_blep_jumps_by_two_across_the_discontinuity() {
    let dt = 0.01;
    let just_before = poly_blep(1.0 - 1e-6, dt);
    let just_after = poly_blep(1e-6, dt);
    assert!(
        (just_before - just_after - 2.0).abs() < 1e-3,
        "expected a jump of -2, got {} -> {}",
        just_before,
        just_after
    );
    // Zero at the kernel edges, so it splices cleanly into the naive waveform.
    assert!(poly_blep(dt - 1e-7, dt).abs() < 1e-4);
    assert!(poly_blep(1.0 - dt + 1e-7, dt).abs() < 1e-4);
}

#[test]
fn poly_blamp_is_continuous_and_meets_one_third_at_the_corner() {
    let dt = 0.01;
    let before = poly_blamp(1.0 - 1e-7, dt);
    let after = poly_blamp(1e-7, dt);
    assert!(
        (before - 1.0 / 3.0).abs() < 1e-4,
        "left limit {before} should be 1/3"
    );
    assert!(
        (after - 1.0 / 3.0).abs() < 1e-4,
        "right limit {after} should be 1/3"
    );
    // Zero at the outer edges.
    assert!(poly_blamp(dt - 1e-7, dt).abs() < 1e-4);
    assert!(poly_blamp(1.0 - dt + 1e-7, dt).abs() < 1e-4);
}

#[test]
fn sine_lut_matches_libm() {
    for i in 0..1000 {
        let phase = i as f32 / 1000.0;
        let expected = (phase * std::f32::consts::TAU).sin();
        let got = osc::sin_lut(phase);
        assert!(
            (got - expected).abs() < 1e-4,
            "phase {phase}: {got} vs {expected}"
        );
    }
    // Wraps correctly for out-of-range input, which phase modulation relies on.
    assert!((osc::sin_lut(3.25) - osc::sin_lut(0.25)).abs() < 1e-5);
    assert!((osc::sin_lut(-0.75) - osc::sin_lut(0.25)).abs() < 1e-5);
}

// ---------------------------------------------------------------------------
// Additive partial bank
// ---------------------------------------------------------------------------

#[test]
fn partial_gains_are_peak_normalised() {
    let sliders = [1.0; N_PARTIALS];
    let g = osc::partial_gains(&sliders, 0.0, 0.0, 0.0, 100.0, SR / 2.0);
    let sum: f32 = g.iter().map(|x| x.abs()).sum();
    // Normalised to unity peak, then the fixed makeup gain.
    assert!((sum - 1.4).abs() < 1e-4, "sum of magnitudes was {sum}");
}

#[test]
fn a_zeroed_drawbar_stays_zero_under_every_macro() {
    let mut sliders = [1.0; N_PARTIALS];
    sliders[4] = 0.0;
    for &tilt in &[-1.0, -0.5, 0.0, 0.5, 1.0] {
        for &decay in &[-60.0, -20.0, 0.0] {
            for &oe in &[-1.0, 0.0, 1.0] {
                let g = osc::partial_gains(&sliders, tilt, decay, oe, 100.0, SR / 2.0);
                assert_eq!(
                    g[4], 0.0,
                    "partial 5 resurrected at tilt={tilt} decay={decay} oe={oe}"
                );
            }
        }
    }
}

#[test]
fn the_fundamental_survives_odd_even() {
    let sliders = [1.0; N_PARTIALS];
    for &oe in &[-1.0, 1.0] {
        let g = osc::partial_gains(&sliders, 0.0, 0.0, oe, 100.0, SR / 2.0);
        assert!(g[0] > 0.0, "fundamental killed at odd_even={oe}");
    }
}

#[test]
fn nyquist_fade_is_continuous_at_both_ends() {
    let nyq = SR / 2.0;
    assert_eq!(osc::nyquist_fade(0.5 * nyq, nyq), 1.0);
    assert_eq!(osc::nyquist_fade(1.2 * nyq, nyq), 0.0);
    // Continuous at the two knees.
    let below = osc::nyquist_fade(0.8999 * nyq, nyq);
    let above = osc::nyquist_fade(0.9001 * nyq, nyq);
    assert!((below - above).abs() < 1e-3);
    let near_top = osc::nyquist_fade(0.9999 * nyq, nyq);
    assert!(near_top.abs() < 1e-3, "fade should reach zero, got {near_top}");
}

#[test]
fn partials_above_nyquist_are_muted() {
    let sliders = [1.0; N_PARTIALS];
    // At 4 kHz the 8th partial is 32 kHz, well past a 24 kHz Nyquist.
    let g = osc::partial_gains(&sliders, 0.0, 0.0, 0.0, 4000.0, SR / 2.0);
    assert_eq!(g[7], 0.0, "8th partial should be muted");
    assert!(g[0] > 0.0, "fundamental should survive");
}

// ---------------------------------------------------------------------------
// Envelopes
// ---------------------------------------------------------------------------

#[test]
fn attack_reaches_full_scale_in_the_configured_time() {
    for &ms in &[1.0_f32, 10.0, 100.0, 1000.0] {
        let mut env = Adsr::default();
        env.set_params(ms, 1000.0, 1.0, 100.0, 0.5, 1.0, SR);
        env.trigger();

        let expected = (ms * 1e-3 * SR).round() as usize;
        let mut reached = None;
        for n in 0..(expected * 3 + 16) {
            env.next();
            if env.stage != EnvStage::Attack && reached.is_none() {
                reached = Some(n + 1);
                break;
            }
        }
        let reached = reached.unwrap_or_else(|| panic!("attack never completed for {ms} ms"));
        let error = (reached as f32 - expected as f32).abs() / expected as f32;
        assert!(
            error < 0.02,
            "{ms} ms attack finished in {reached} samples, expected {expected}"
        );
    }
}

#[test]
fn release_ends_and_reports_finished() {
    let mut env = Adsr::default();
    env.set_params(1.0, 1.0, 0.8, 50.0, 0.5, 1.0, SR);
    env.trigger();
    for _ in 0..(SR as usize / 10) {
        env.next();
    }
    env.release();
    let mut n = 0;
    while env.stage != EnvStage::Finished && n < SR as usize {
        env.next();
        n += 1;
    }
    assert_eq!(env.stage, EnvStage::Finished, "release never finished");
    assert_eq!(env.out, 0.0);
    // 50 ms release, allowing for the exponential tail hitting the epsilon.
    assert!(n < (0.2 * SR) as usize, "release took {n} samples");
}

#[test]
fn envelope_output_stays_bounded() {
    let mut env = Adsr::default();
    env.set_params(0.5, 5.0, 0.5, 5.0, 1.0, 1.0, SR);
    env.trigger();
    for _ in 0..10_000 {
        let v = env.next();
        assert!((0.0..=1.0).contains(&v), "envelope left [0,1]: {v}");
    }
}

// ---------------------------------------------------------------------------
// Filter
// ---------------------------------------------------------------------------

#[test]
fn ladder_is_stable_at_full_resonance() {
    let mut ladder = Ladder::ideal();
    ladder.snap_gain(Ladder::gain_for(1000.0, SR));
    let k = filter::resonance_to_k(1.0);
    let taps = filter::mode_taps(FilterMode::Lp24);
    let mut rng = Rng32::new(7);

    let mut peak = 0.0f32;
    for _ in 0..(SR as usize * 2) {
        let y = ladder.next(rng.bipolar() * 0.5, k, 0.6, &taps);
        assert!(y.is_finite(), "ladder produced a non-finite sample");
        peak = peak.max(y.abs());
    }
    assert!(peak < 20.0, "ladder resonance ran away, peak {peak}");
}

#[test]
fn ladder_lowpass_attenuates_above_cutoff() {
    // Drive a sine well above cutoff and check it is strongly attenuated.
    let cutoff = 500.0;
    let probe = 8000.0;
    let mag = filter::magnitude(FilterMode::Lp24, cutoff, 0.0, probe, SR);
    let dc = filter::magnitude(FilterMode::Lp24, cutoff, 0.0, 20.0, SR);
    let db = 20.0 * (mag / dc).log10();
    assert!(db < -40.0, "24 dB/oct lowpass only gave {db} dB at 16x cutoff");
}

#[test]
fn ladder_highpass_attenuates_below_cutoff() {
    let cutoff = 2000.0;
    let mag = filter::magnitude(FilterMode::Hp24, cutoff, 0.0, 60.0, SR);
    let pass = filter::magnitude(FilterMode::Hp24, cutoff, 0.0, 18_000.0, SR);
    let db = 20.0 * (mag / pass).log10();
    assert!(db < -40.0, "highpass only gave {db} dB below cutoff");
}

#[test]
fn ladder_self_oscillates_from_a_noise_seed() {
    let mut ladder = Ladder::ideal();
    ladder.snap_gain(Ladder::gain_for(800.0, SR));
    let k = filter::resonance_to_k(1.0);
    let taps = filter::mode_taps(FilterMode::Lp24);
    let mut rng = Rng32::new(11);

    // Seed noise only, exactly as the voice does at analog == 0.
    let mut peak = 0.0f32;
    for n in 0..(SR as usize * 2) {
        let y = ladder.next(rng.bipolar() * 1e-5, k, 0.6, &taps);
        if n > SR as usize {
            peak = peak.max(y.abs());
        }
    }
    assert!(
        peak > 1e-4,
        "resonance never built up from the noise seed, peak {peak}"
    );
}

// ---------------------------------------------------------------------------
// FM
// ---------------------------------------------------------------------------

#[test]
fn carson_limiter_keeps_sidebands_below_nyquist() {
    for &f_mod in &[100.0_f32, 1000.0, 5000.0, 12_000.0] {
        let max_cycles = fm::max_index_cycles(f_mod, SR);
        let beta = max_cycles * std::f32::consts::TAU;
        let top = f_mod * (beta + 1.0);
        assert!(
            top <= 0.45 * SR + 1.0,
            "f_mod={f_mod}: sidebands reach {top} Hz"
        );
    }
}

#[test]
fn carson_limiter_shuts_down_above_nyquist() {
    assert_eq!(fm::max_index_cycles(40_000.0, SR), 0.0);
}

#[test]
fn fm_env_multiplier_stays_in_range() {
    for &amount in &[-1.0_f32, -0.5, 0.0, 0.5, 1.0] {
        for step in 0..=10 {
            let env = step as f32 / 10.0;
            let m = fm::env_multiplier(amount, env);
            assert!(
                (-0.001..=1.001).contains(&m),
                "multiplier {m} out of range at amount={amount} env={env}"
            );
        }
    }
    // Fully enveloped and fully open means the index passes through unchanged.
    assert!((fm::env_multiplier(1.0, 1.0) - 1.0).abs() < 1e-6);
    assert!(fm::env_multiplier(1.0, 0.0).abs() < 1e-6);
}

#[test]
fn harmonic_mode_snaps_to_the_ratio_table() {
    use synth::params::FmRatioMode;
    let hz = fm::modulator_hz(FmRatioMode::Harmonic, 2.9, 0.0, 440.0, 100.0);
    assert!((hz - 300.0).abs() < 1e-3, "2.9 should snap to 3, got {hz}");
    let fixed = fm::modulator_hz(FmRatioMode::Fixed, 2.9, 0.0, 440.0, 100.0);
    assert_eq!(fixed, 440.0);
}

// ---------------------------------------------------------------------------
// Analog character
// ---------------------------------------------------------------------------

#[test]
fn tolerances_are_exactly_neutral_when_analog_is_zero() {
    let mut rng = Rng32::new(3);
    let t = Tolerance::draw(&mut rng, 0.0);
    assert_eq!(t.pitch_cents, [0.0, 0.0]);
    assert_eq!(t.gain, 1.0);
    assert_eq!(t.stage_mismatch, [1.0; 4]);
    assert_eq!(t.env_scale, [1.0; 3]);
}

#[test]
fn tolerances_stay_within_their_documented_bounds() {
    for seed in 1..200u32 {
        let mut rng = Rng32::new(seed);
        let t = Tolerance::draw(&mut rng, 1.0);
        assert!(t.pitch_cents.iter().all(|c| c.abs() <= 5.0));
        assert!(t.cutoff_st.abs() <= 1.2);
        assert!(t.resonance.abs() <= 0.04);
        assert!(t.env_scale.iter().all(|s| (0.92..=1.08).contains(s)));
        assert!(t.stage_mismatch.iter().all(|s| (0.98..=1.02).contains(s)));
        assert!(t.pan_offset.abs() <= 0.06);
    }
}

#[test]
fn rng_covers_its_range_without_repeating_immediately() {
    let mut rng = Rng32::new(42);
    let mut min = f32::MAX;
    let mut max = f32::MIN;
    let mut sum = 0.0f64;
    const N: usize = 100_000;
    for _ in 0..N {
        let v = rng.bipolar();
        assert!((-1.0..1.0).contains(&v));
        min = min.min(v);
        max = max.max(v);
        sum += v as f64;
    }
    assert!(min < -0.99 && max > 0.99, "poor coverage: {min}..{max}");
    assert!((sum / N as f64).abs() < 0.01, "biased mean {}", sum / N as f64);
}

// ---------------------------------------------------------------------------
// Panning
// ---------------------------------------------------------------------------

#[test]
fn constant_power_pan_holds_across_the_range() {
    for i in 0..=200 {
        let pan = -1.0 + 2.0 * i as f32 / 200.0;
        let theta = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
        let (l, r) = (theta.cos(), theta.sin());
        let power = l * l + r * r;
        assert!(
            (power - 1.0).abs() < 1e-5,
            "pan {pan} gave power {power}"
        );
    }
    // Centre is -3 dB in each channel.
    let centre = (std::f32::consts::FRAC_PI_4).cos();
    assert!((centre - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
}
