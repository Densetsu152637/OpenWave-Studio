//! Offline analysis against signals generated here with known answers: loudness (EBU Tech 3341
//! style cases and other sample rates), gating, true peak, levels, correlation, band energies
//! and normalisation.

use std::f64::consts::TAU;

use ow_media::analysis::{self, analyze, integrated_loudness, k_weighting, normalize, true_peak};

const RATE: u32 = 48_000;

/// Interleaved stereo sine of `freq` at `dbfs` peak in both channels.
fn tone(freq: f64, dbfs: f64, seconds: f64, rate: u32) -> Vec<f32> {
    tone_phase(freq, dbfs, seconds, rate, 0.0)
}

fn tone_phase(freq: f64, dbfs: f64, seconds: f64, rate: u32, phase: f64) -> Vec<f32> {
    let amplitude = 10.0_f64.powf(dbfs / 20.0);
    let frames = (seconds * f64::from(rate)).round() as usize;
    let mut out = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let s = (amplitude * (TAU * freq * i as f64 / f64::from(rate) + phase).sin()) as f32;
        out.push(s);
        out.push(s);
    }
    out
}

fn silence(seconds: f64, rate: u32) -> Vec<f32> {
    vec![0.0; (seconds * f64::from(rate)).round() as usize * 2]
}

fn close(value: Option<f64>, expected: f64, tolerance: f64, what: &str) {
    let value = value.unwrap_or_else(|| panic!("{what}: silent"));
    assert!(
        (value - expected).abs() <= tolerance,
        "{what}: {value} vs {expected} (tolerance {tolerance})"
    );
}

/// A 1 kHz stereo sine reads its peak level in dB as LUFS (the K-weighting is calibrated to
/// that: two channels of a full-scale sine are 0 LUFS).
fn level(lufs: f64, seconds: f64) -> Vec<f32> {
    tone(1000.0, lufs, seconds, RATE)
}

#[test]
fn analysis_k_weighting_matches_the_standard_at_48_khz() {
    let [shelf, high_pass] = k_weighting(48_000);
    let near = |a: f64, b: f64| assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    near(shelf.b[0], 1.535_124_859_586_97);
    near(shelf.b[1], -2.691_696_189_406_38);
    near(shelf.b[2], 1.198_392_810_852_85);
    near(shelf.a[0], -1.690_659_293_182_41);
    near(shelf.a[1], 0.732_480_774_215_85);
    near(high_pass.b[0], 1.0);
    near(high_pass.b[1], -2.0);
    near(high_pass.a[0], -1.990_047_454_833_98);
    near(high_pass.a[1], 0.990_072_250_366_21);
}

#[test]
fn analysis_ebu_3341_case_1_and_2_steady_tones() {
    for target in [-23.0, -33.0] {
        let report = analyze(&level(target, 20.0), RATE);
        close(report.integrated_lufs, target, 0.1, "integrated");
        close(report.momentary_max_lufs, target, 0.1, "momentary max");
        close(report.short_term_max_lufs, target, 0.1, "short-term max");
    }
}

#[test]
fn analysis_ebu_3341_case_3_quiet_ends_are_gated_out() {
    // 10 s at -36 LUFS, 60 s at -23, 10 s at -36: the quiet parts fall below the relative gate.
    let mut signal = level(-36.0, 10.0);
    signal.extend(level(-23.0, 60.0));
    signal.extend(level(-36.0, 10.0));
    close(integrated_loudness(&signal, RATE), -23.0, 0.1, "case 3");
}

#[test]
fn analysis_ebu_3341_case_4_absolute_gate() {
    // Levels below -70 LUFS never count: 10 s at -72, 10 s at -36, 60 s at -23, 10 s at -36,
    // 10 s at -72.
    let mut signal = level(-72.0, 10.0);
    signal.extend(level(-36.0, 10.0));
    signal.extend(level(-23.0, 60.0));
    signal.extend(level(-36.0, 10.0));
    signal.extend(level(-72.0, 10.0));
    close(integrated_loudness(&signal, RATE), -23.0, 0.1, "case 4");
}

#[test]
fn analysis_ebu_3341_case_5_relative_gate_keeps_close_levels() {
    // 20 s at -26, 20.1 s at -20, 20 s at -26 are all within 10 LU of their mean: -23.0.
    let mut signal = level(-26.0, 20.0);
    signal.extend(level(-20.0, 20.1));
    signal.extend(level(-26.0, 20.0));
    close(integrated_loudness(&signal, RATE), -23.0, 0.1, "case 5");
}

#[test]
fn analysis_loudness_holds_at_other_sample_rates() {
    for rate in [44_100, 88_200, 96_000] {
        let signal = tone(1000.0, -23.0, 10.0, rate);
        close(
            integrated_loudness(&signal, rate),
            -23.0,
            0.1,
            &format!("{rate} Hz"),
        );
    }
}

#[test]
fn analysis_k_weighting_shapes_the_response() {
    // Relative to 1 kHz (which reads its own level) the RLB high-pass takes about 1.1 dB off
    // 100 Hz and 13 dB off 20 Hz, and the shelf adds about 3.3 dB at 10 kHz (4 dB there, less
    // the 0.7 dB the 1 kHz reference already has).
    let at = |freq| integrated_loudness(&tone(freq, -20.0, 10.0, RATE), RATE).unwrap();
    assert!(
        (at(100.0) - (-20.0 - 1.17 - 0.69)).abs() < 0.15,
        "{}",
        at(100.0)
    );
    assert!(
        (at(10_000.0) - (-20.0 + 4.0 - 0.69)).abs() < 0.3,
        "{}",
        at(10_000.0)
    );
    assert!(at(20.0) < -20.0 - 12.0, "{}", at(20.0));
}

#[test]
fn analysis_momentary_and_short_term_maxima_follow_the_loudest_window() {
    // 10 s at -33 then 10 s at -23: both windows end up at -23.
    let mut signal = level(-33.0, 10.0);
    signal.extend(level(-23.0, 10.0));
    let report = analyze(&signal, RATE);
    close(report.momentary_max_lufs, -23.0, 0.1, "momentary");
    close(report.short_term_max_lufs, -23.0, 0.1, "short-term");
    // A 400 ms burst in silence: the momentary window holds it whole, the 3 s window averages
    // it down by 10 log10(3 / 0.4) = 8.75 dB.
    let mut burst = silence(1.0, RATE);
    burst.extend(level(-20.0, 0.4));
    burst.extend(silence(4.0, RATE));
    let report = analyze(&burst, RATE);
    close(report.momentary_max_lufs, -20.0, 0.1, "burst momentary");
    close(
        report.short_term_max_lufs,
        -20.0 - 8.75,
        0.1,
        "burst short-term",
    );
}

#[test]
fn analysis_true_peak_sees_intersample_peaks() {
    // Sine at a quarter of the sample rate with a 45 degree phase: every sample is +-0.707 of
    // the amplitude, so the sample peak is 3 dB under the true peak.
    for (rate, dbfs) in [(48_000, -6.0), (44_100, -1.0), (96_000, -12.0)] {
        let signal = tone_phase(f64::from(rate) / 4.0, dbfs, 1.0, rate, TAU / 8.0);
        let report = analyze(&signal, rate);
        close(
            report.true_peak_dbtp,
            dbfs,
            0.2,
            &format!("{rate} true peak"),
        );
        close(
            report.sample_peak_db,
            dbfs - 3.0103,
            0.05,
            &format!("{rate} sample peak"),
        );
    }
}

#[test]
fn analysis_true_peak_matches_the_sample_peak_for_low_frequencies() {
    let signal = tone(997.0, -6.0, 2.0, RATE);
    let report = analyze(&signal, RATE);
    close(report.true_peak_dbtp, -6.0, 0.05, "997 Hz");
    // Never below the sample peak.
    assert!(report.true_peak_dbtp.unwrap() >= report.sample_peak_db.unwrap() - 1e-9);
    // A negative, one-channel peak counts too, at the very start and end of the buffer.
    let mut edge = vec![0.0_f32; 2000];
    edge[1] = -0.5;
    edge[1998] = 0.25;
    assert!((true_peak(&edge) - 0.5).abs() < 0.02);
}

#[test]
fn analysis_levels_rms_crest_and_correlation() {
    let report = analyze(&tone(440.0, -6.0, 2.0, RATE), RATE);
    close(report.rms_db, -6.0 - 3.0103, 0.01, "rms");
    close(report.crest_factor_db, 3.0103, 0.02, "crest");
    close(Some(report.correlation), 1.0, 1e-9, "mono correlation");

    let mut inverted = tone(440.0, -6.0, 2.0, RATE);
    for pair in inverted.as_chunks_mut::<2>().0 {
        pair[1] = -pair[1];
    }
    close(
        Some(analyze(&inverted, RATE).correlation),
        -1.0,
        1e-9,
        "inverted",
    );

    // Two tones of different frequency over whole cycles are uncorrelated.
    let (left, right) = (tone(1000.0, -6.0, 1.0, RATE), tone(1500.0, -6.0, 1.0, RATE));
    let apart: Vec<f32> = left
        .as_chunks::<2>()
        .0
        .iter()
        .zip(right.as_chunks::<2>().0)
        .flat_map(|(l, r)| [l[0], r[0]])
        .collect();
    close(
        Some(analyze(&apart, RATE).correlation),
        0.0,
        0.01,
        "independent",
    );
}

#[test]
fn analysis_band_energies_follow_the_spectrum() {
    // 100 Hz at 0.3 (band 1: 60-150) and 5 kHz at 0.2 (band 5: 2.5-6 kHz), in both channels.
    let low = tone(100.0, 20.0 * 0.3_f64.log10(), 3.0, RATE);
    let high = tone(5000.0, 20.0 * 0.2_f64.log10(), 3.0, RATE);
    let signal: Vec<f32> = low.iter().zip(&high).map(|(a, b)| a + b).collect();
    let report = analyze(&signal, RATE);
    let bands = report.band_energies_db;
    close(
        bands[1],
        10.0 * (0.3_f64 * 0.3 / 2.0).log10(),
        0.2,
        "100 Hz band",
    );
    close(
        bands[5],
        10.0 * (0.2_f64 * 0.2 / 2.0).log10(),
        0.2,
        "5 kHz band",
    );
    for (index, band) in bands.iter().enumerate() {
        if index != 1 && index != 5 {
            assert!(band.is_none_or(|db| db < -50.0), "band {index}: {band:?}");
        }
    }
    // The bands add up to the total mean square.
    let total: f64 = bands
        .iter()
        .flatten()
        .map(|db| 10.0_f64.powf(db / 10.0))
        .sum();
    let rms = 10.0_f64.powf(report.rms_db.unwrap() / 10.0);
    assert!(
        (10.0 * (total / rms).log10()).abs() < 0.2,
        "{total} vs {rms}"
    );
    assert_eq!(analysis::BAND_EDGES_HZ.len(), bands.len());
}

#[test]
fn analysis_silence_and_short_inputs_do_not_panic() {
    let empty = analyze(&[], RATE);
    assert_eq!(empty.frames, 0);
    assert_eq!(empty.integrated_lufs, None);
    assert_eq!(empty.true_peak_dbtp, None);
    let quiet = analyze(&silence(2.0, RATE), RATE);
    assert_eq!(quiet.integrated_lufs, None);
    assert_eq!(quiet.sample_peak_db, None);
    assert_eq!(quiet.rms_db, None);
    assert_eq!(quiet.correlation, 0.0);
    assert!(quiet.band_energies_db.iter().all(Option::is_none));
    // Shorter than one momentary block: measured over what there is.
    close(
        analyze(&level(-23.0, 0.3), RATE).momentary_max_lufs,
        -23.0,
        0.5,
        "0.3 s",
    );
    let tiny = analyze(&[0.5, 0.5, -0.5, -0.5], RATE);
    assert_eq!(tiny.frames, 2);
    assert!(tiny.sample_peak_db.is_some());
    // Non-finite samples do not poison the loudness.
    let mut bad = level(-23.0, 2.0);
    bad[10] = f32::NAN;
    assert!(analyze(&bad, RATE).integrated_lufs.is_some());
}

#[test]
fn analysis_normalize_reaches_the_target_loudness() {
    let mut signal = level(-30.0, 10.0);
    let done = normalize(&mut signal, RATE, -14.0, -1.0);
    assert!(!done.limited_by_ceiling);
    close(Some(done.gain_db), 16.0, 0.1, "gain");
    close(done.loudness_before, -30.0, 0.1, "before");
    close(done.loudness_after, -14.0, 0.1, "after");
    close(
        analyze(&signal, RATE).integrated_lufs,
        -14.0,
        0.1,
        "measured",
    );
}

#[test]
fn analysis_normalize_stops_at_the_true_peak_ceiling() {
    // A quiet sine with one loud spike: the target would push the spike over the ceiling.
    let mut signal = level(-30.0, 10.0);
    signal[48_000] = 0.7;
    signal[48_001] = 0.7;
    let before = analyze(&signal, RATE);
    let done = normalize(&mut signal, RATE, -14.0, -1.0);
    assert!(done.limited_by_ceiling);
    assert!(done.gain_db < 16.0 - 1.0);
    close(done.true_peak_after, -1.0, 0.01, "true peak after");
    close(
        Some(done.gain_db),
        -1.0 - before.true_peak_dbtp.unwrap(),
        1e-6,
        "gain",
    );
    assert!(done.loudness_after.unwrap() < -14.0);
}

#[test]
fn analysis_normalize_leaves_silence_alone() {
    let mut signal = silence(1.0, RATE);
    let done = normalize(&mut signal, RATE, -14.0, -1.0);
    assert_eq!(done.gain_db, 0.0);
    assert!(signal.iter().all(|s| *s == 0.0));
    assert_eq!(done.loudness_after, None);
}
