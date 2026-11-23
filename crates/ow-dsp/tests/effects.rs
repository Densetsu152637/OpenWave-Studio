//! The built-in effects: EQ band gain, compressor static curve and timing, reverb stability and
//! decay, registry entries and realtime safety.

use std::f64::consts::TAU;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use ow_dsp::effects::{compressor, eq, limiter, reverb};
use ow_dsp::{Device, DeviceInfo, find};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const RATE: f32 = 48_000.0;

fn make(kind: &str, rate: f32) -> Box<dyn Device> {
    find(kind).expect("registered").create(rate, 4096)
}

fn sine(freq: f64, amplitude: f64, frames: usize, rate: f32) -> Vec<f32> {
    (0..frames)
        .map(|i| (amplitude * (TAU * freq * i as f64 / f64::from(rate)).sin()) as f32)
        .collect()
}

fn run(device: &mut dyn Device, left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (left.to_vec(), right.to_vec());
    device.process(&mut [&mut l, &mut r], &[]);
    (l, r)
}

fn rms(signal: &[f32]) -> f64 {
    (signal.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / signal.len() as f64).sqrt()
}

/// Gain in decibels of `device` at `freq`, from a steady sine (the first half is discarded).
fn gain_db(device: &mut dyn Device, freq: f64, rate: f32) -> f64 {
    let input = sine(freq, 0.25, rate as usize, rate);
    let (out, _) = run(device, &input, &input);
    let half = input.len() / 2;
    20.0 * (rms(&out[half..]) / rms(&input[half..])).log10()
}

fn eq_with(settings: &[(u32, f64)], rate: f32) -> Box<dyn Device> {
    let mut device = make(eq::KIND, rate);
    for (id, value) in settings {
        device.set_param(*id, *value);
    }
    device
}

#[test]
fn effects_are_registered_as_non_instruments() {
    for kind in [eq::KIND, compressor::KIND, reverb::KIND, limiter::KIND] {
        let info: &DeviceInfo = find(kind).expect("registered");
        assert!(!info.instrument, "{kind}");
        assert!(!info.params.is_empty());
        for (n, param) in info.params.iter().enumerate() {
            assert!(param.min <= param.default && param.default <= param.max);
            assert!(
                info.params[..n].iter().all(|p| p.id != param.id),
                "unique ids"
            );
            if param.scale == ow_dsp::Scale::Log {
                assert!(param.min > 0.0);
            }
        }
    }
}

#[test]
fn effects_eq_is_transparent_when_flat() {
    let mut device = make(eq::KIND, RATE);
    let input = sine(440.0, 0.5, 4800, RATE);
    let other = sine(1234.0, 0.25, 4800, RATE);
    let (l, r) = run(&mut *device, &input, &other);
    assert_eq!(l, input);
    assert_eq!(r, other);
}

#[test]
fn effects_eq_peak_reaches_its_gain_at_the_centre_frequency() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        for (freq, gain, q) in [
            (1_000.0, 9.0, 1.0),
            (250.0, -12.0, 2.0),
            (6_000.0, 6.0, 0.5),
        ] {
            let mut device = eq_with(
                &[
                    (eq::PARAM_MID_FREQ, freq),
                    (eq::PARAM_MID_GAIN, gain),
                    (eq::PARAM_MID_Q, q),
                ],
                rate,
            );
            let measured = gain_db(&mut *device, freq, rate);
            assert!(
                (measured - gain).abs() < 0.1,
                "{freq} Hz at {rate}: wanted {gain} dB, got {measured}"
            );
        }
    }
}

#[test]
fn effects_eq_shelves_hit_half_gain_at_the_corner_and_full_gain_beyond() {
    let mut low = eq_with(
        &[(eq::PARAM_LOW_FREQ, 200.0), (eq::PARAM_LOW_GAIN, 12.0)],
        RATE,
    );
    let at_corner = gain_db(&mut *low, 200.0, RATE);
    assert!((at_corner - 6.0).abs() < 0.1, "{at_corner}");
    low.reset();
    assert!((gain_db(&mut *low, 20.0, RATE) - 12.0).abs() < 0.1);
    low.reset();
    assert!(gain_db(&mut *low, 5_000.0, RATE).abs() < 0.1);

    let mut high = eq_with(
        &[(eq::PARAM_HIGH_FREQ, 4_000.0), (eq::PARAM_HIGH_GAIN, -10.0)],
        RATE,
    );
    let at_corner = gain_db(&mut *high, 4_000.0, RATE);
    assert!((at_corner + 5.0).abs() < 0.1, "{at_corner}");
    high.reset();
    assert!((gain_db(&mut *high, 16_000.0, RATE) + 10.0).abs() < 0.2);
    high.reset();
    assert!(gain_db(&mut *high, 100.0, RATE).abs() < 0.1);
}

#[test]
fn effects_eq_bands_combine_and_channels_are_independent() {
    let mut device = eq_with(
        &[
            (eq::PARAM_LOW_GAIN, 6.0),
            (eq::PARAM_MID_GAIN, -6.0),
            (eq::PARAM_HIGH_GAIN, 6.0),
        ],
        RATE,
    );
    let input = sine(1_000.0, 0.25, 48_000, RATE);
    let silence = vec![0.0; input.len()];
    let (l, r) = run(&mut *device, &input, &silence);
    assert!(
        r.iter().all(|s| *s == 0.0),
        "the right channel stays silent"
    );
    let measured = 20.0 * (rms(&l[24_000..]) / rms(&input[24_000..])).log10();
    // The mid cut dominates at 1 kHz; the shelves leak a little in.
    assert!((-6.5..-3.5).contains(&measured), "{measured}");
}

/// Output level in decibels for a square wave of `level_db` (every sample has that magnitude).
fn compressed_db(device: &mut dyn Device, level_db: f64) -> f64 {
    let amplitude = 10.0_f64.powf(level_db / 20.0) as f32;
    let input: Vec<f32> = (0..96_000)
        .map(|i| {
            if i / 37 % 2 == 0 {
                amplitude
            } else {
                -amplitude
            }
        })
        .collect();
    let (out, _) = run(device, &input, &input);
    20.0 * f64::from(out[out.len() - 1].abs()).log10()
}

#[test]
fn effects_compressor_static_curve_matches_the_gain_computer() {
    for (threshold, ratio, makeup) in [(-20.0, 4.0, 0.0), (-30.0, 2.0, 6.0), (-12.0, 10.0, 3.0)] {
        for level in [-60.0, -40.0, -30.0, -20.0, -12.0, -6.0, 0.0] {
            let mut device = make(compressor::KIND, RATE);
            device.set_param(compressor::PARAM_THRESHOLD, threshold);
            device.set_param(compressor::PARAM_RATIO, ratio);
            device.set_param(compressor::PARAM_MAKEUP, makeup);
            device.set_param(compressor::PARAM_ATTACK, 0.001);
            device.set_param(compressor::PARAM_RELEASE, 0.05);
            let expected = if level > threshold {
                threshold + (level - threshold) / ratio
            } else {
                level
            } + makeup;
            let measured = compressed_db(&mut *device, level);
            assert!(
                (measured - expected).abs() < 0.5,
                "threshold {threshold}, ratio {ratio}, in {level}: wanted {expected}, got {measured}"
            );
        }
    }
}

#[test]
fn effects_compressor_attack_and_release_follow_their_times() {
    let mut device = make(compressor::KIND, RATE);
    device.set_param(compressor::PARAM_THRESHOLD, -20.0);
    device.set_param(compressor::PARAM_RATIO, 20.0);
    device.set_param(compressor::PARAM_ATTACK, 0.05);
    device.set_param(compressor::PARAM_RELEASE, 0.5);
    // A loud steady level with a quiet tail; reduction is read back from the output level.
    let loud = vec![0.5_f32; 48_000];
    let (out, _) = run(&mut *device, &loud, &loud);
    let reduction_db = |sample: f32| -20.0 * f64::from(sample / 0.5).log10();
    let final_reduction = reduction_db(out[out.len() - 1]);
    assert!(final_reduction > 10.0, "{final_reduction}");
    // One attack time in, the reduction is about 63 % of its final value.
    let at_attack = reduction_db(out[2_400]);
    let fraction = at_attack / final_reduction;
    assert!((0.55..0.72).contains(&fraction), "{fraction}");
    // The first sample is nearly untouched (the reduction builds up, not a hard clamp).
    assert!(reduction_db(out[0]) < 1.0);

    // Release: after the level drops, the gain recovers with the release time constant.
    let quiet = vec![0.001_f32; 48_000];
    let (out, _) = run(&mut *device, &quiet, &quiet);
    let remaining = |sample: f32| -20.0 * f64::from(sample / 0.001).log10();
    let start = remaining(out[0]);
    let after_release_time = remaining(out[24_000]);
    assert!(
        (after_release_time / start - 0.37).abs() < 0.08,
        "{start} then {after_release_time}"
    );
}

#[test]
fn effects_compressor_links_the_channels() {
    let mut device = make(compressor::KIND, RATE);
    device.set_param(compressor::PARAM_ATTACK, 0.0001);
    let loud = vec![0.8_f32; 24_000];
    let quiet = vec![0.01_f32; 24_000];
    let (l, r) = run(&mut *device, &loud, &quiet);
    let gain_l = l[l.len() - 1] / 0.8;
    let gain_r = r[r.len() - 1] / 0.01;
    assert!(gain_l < 0.7);
    assert!((gain_l - gain_r).abs() < 1e-6, "one gain for both channels");
}

#[test]
fn effects_reverb_with_no_mix_is_the_dry_signal() {
    let mut device = make(reverb::KIND, RATE);
    device.set_param(reverb::PARAM_MIX, 0.0);
    let input = sine(300.0, 0.5, 4800, RATE);
    let (l, r) = run(&mut *device, &input, &input);
    assert_eq!(l, input);
    assert_eq!(r, input);
}

/// Energy of `signal` in windows of `window` frames.
fn energies(signal: &[f32], window: usize) -> Vec<f64> {
    signal
        .chunks(window)
        .map(|chunk| chunk.iter().map(|s| f64::from(*s).powi(2)).sum())
        .collect()
}

#[test]
fn effects_reverb_tail_is_finite_audible_and_decays() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        for (size, damping) in [(0.0, 0.0), (50.0, 50.0), (100.0, 0.0), (100.0, 100.0)] {
            let mut device = make(reverb::KIND, rate);
            device.set_param(reverb::PARAM_SIZE, size);
            device.set_param(reverb::PARAM_DAMPING, damping);
            device.set_param(reverb::PARAM_MIX, 100.0);
            let frames = rate as usize * 20;
            let mut left = vec![0.0_f32; frames];
            left[0] = 1.0;
            let mut right = left.clone();
            device.process(&mut [&mut left, &mut right], &[]);
            assert!(left.iter().chain(&right).all(|s| s.is_finite()));

            let window = rate as usize / 2;
            let tail = energies(&left, window);
            assert!(
                tail[1] > 1.0e-6,
                "the tail is audible: {size}/{damping} {tail:?}"
            );
            // Energy per half second falls over time (compare 2 s steps to ignore ripples).
            for pair in tail.windows(5).step_by(4).take(6) {
                assert!(
                    pair[4] <= pair[0],
                    "{size}/{damping}: later {} > earlier {}",
                    pair[4],
                    pair[0]
                );
            }
            assert!(
                *tail.last().unwrap() < tail[1] * 0.2,
                "{size}/{damping} still ringing: {tail:?}"
            );
            assert_ne!(left, right, "the channels are decorrelated");
        }
    }
}

#[test]
fn effects_reverb_larger_sizes_ring_longer_and_reset_silences() {
    let tail_energy = |size: f64| {
        let mut device = make(reverb::KIND, RATE);
        device.set_param(reverb::PARAM_SIZE, size);
        device.set_param(reverb::PARAM_MIX, 100.0);
        let mut left = vec![0.0_f32; 96_000];
        left[0] = 1.0;
        let mut right = left.clone();
        device.process(&mut [&mut left, &mut right], &[]);
        left[48_000..]
            .iter()
            .map(|s| f64::from(*s).powi(2))
            .sum::<f64>()
    };
    assert!(tail_energy(90.0) > 10.0 * tail_energy(20.0));

    let mut device = make(reverb::KIND, RATE);
    device.set_param(reverb::PARAM_MIX, 100.0);
    let mut impulse = vec![0.0_f32; 4800];
    impulse[0] = 1.0;
    let mut other = impulse.clone();
    device.process(&mut [&mut impulse, &mut other], &[]);
    device.reset();
    let silence = vec![0.0; 4800];
    let (l, r) = run(&mut *device, &silence, &silence);
    assert!(l.iter().chain(&r).all(|s| *s == 0.0));
}

#[test]
fn effects_stay_finite_on_extreme_input_and_parameters() {
    for info in [eq::INFO, compressor::INFO, reverb::INFO, limiter::INFO] {
        for pick in [0, 1] {
            let mut device = info.create(RATE, 4096);
            for param in info.params {
                let value = if pick == 0 { param.min } else { param.max };
                device.set_param(param.id, value);
            }
            let mut left: Vec<f32> = (0..4096)
                .map(|i| if i % 3 == 0 { 1.0 } else { -1.0 })
                .collect();
            let mut right = vec![0.0; 4096];
            for _ in 0..50 {
                device.process(&mut [&mut left, &mut right], &[]);
                assert!(
                    left.iter().chain(&right).all(|s| s.is_finite()),
                    "{} pick {pick}",
                    info.kind
                );
                // Feed the output back in, with a hot input, to stress stability.
                left.iter_mut().for_each(|s| *s = s.clamp(-1.0, 1.0));
            }
        }
    }
}

#[test]
fn effects_do_not_allocate_while_processing() {
    for info in [eq::INFO, compressor::INFO, reverb::INFO, limiter::INFO] {
        let mut device = info.create(RATE, 512);
        let mut left = sine(220.0, 0.8, 512, RATE);
        let mut right = sine(330.0, 0.8, 512, RATE);
        for block in 0..200 {
            assert_no_alloc(|| {
                if block % 20 == 0 {
                    for param in info.params {
                        let t = f64::from(block % 7) / 6.0;
                        device.set_param(param.id, param.min + (param.max - param.min) * t);
                    }
                }
                if block % 100 == 99 {
                    device.reset();
                }
                device.process(&mut [&mut left, &mut right], &[]);
            });
            assert!(left.iter().all(|s| s.is_finite()), "{}", info.kind);
            left.iter_mut().for_each(|s| *s = s.clamp(-1.0, 1.0));
        }
    }
}

fn limiter_with(ceiling_db: f64, release: f64, lookahead: f64, rate: f32) -> Box<dyn Device> {
    let mut device = make(limiter::KIND, rate);
    device.set_param(limiter::PARAM_CEILING, ceiling_db);
    device.set_param(limiter::PARAM_RELEASE, release);
    device.set_param(limiter::PARAM_LOOKAHEAD, lookahead);
    device
}

#[test]
fn effects_limiter_reports_its_lookahead_as_latency() {
    for (rate, lookahead, expected) in [
        (48_000.0, 0.002, 96),
        (48_000.0, 0.005, 240),
        (44_100.0, 0.001, 44),
        (96_000.0, 0.003, 288),
    ] {
        let device = limiter_with(-1.0, 0.1, lookahead, rate);
        assert_eq!(device.latency_frames(), expected, "{rate} {lookahead}");
    }
    // Every other effect adds none.
    for kind in [eq::KIND, compressor::KIND, reverb::KIND] {
        assert_eq!(make(kind, RATE).latency_frames(), 0, "{kind}");
    }
}

#[test]
fn effects_limiter_never_exceeds_its_ceiling_and_delays_by_the_latency() {
    for (ceiling_db, lookahead) in [(-1.0, 0.002), (-6.0, 0.005), (-0.1, 0.001), (-20.0, 0.003)] {
        let ceiling = 10.0_f64.powf(ceiling_db / 20.0) as f32;
        let mut device = limiter_with(ceiling_db, 0.05, lookahead, RATE);
        let latency = device.latency_frames() as usize;
        // A loud, uneven signal with isolated spikes and a hard clipped burst.
        let mut left = sine(180.0, 1.8, 24_000, RATE);
        let mut right = sine(230.0, 1.4, 24_000, RATE);
        left[5000] = 4.0;
        right[9000] = -6.0;
        for sample in &mut left[14_000..14_300] {
            *sample = 3.0 * sample.signum();
        }
        for chunk in 0..(24_000 / 500) {
            let range = chunk * 500..(chunk + 1) * 500;
            let (l, r) = (&mut left[range.clone()], &mut right[range]);
            device.process(&mut [l, r], &[]);
        }
        let peak = left
            .iter()
            .chain(&right)
            .fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!(
            peak <= ceiling,
            "peak {peak} above {ceiling} ({ceiling_db} dB)"
        );
        // Reduction is not wasted: the output is loud, and the first frames are the delay's
        // silence.
        assert!(peak > ceiling * 0.95, "peak {peak}");
        assert!(left[..latency].iter().all(|s| *s == 0.0));
        // A quiet passage far from any peak passes unchanged, delayed by the latency.
        let quiet_in = sine(300.0, 0.05, 6000, RATE);
        let mut device = limiter_with(ceiling_db.max(-6.0), 0.05, lookahead, RATE);
        let latency = device.latency_frames() as usize;
        let (mut l, mut r) = (quiet_in.clone(), quiet_in.clone());
        device.process(&mut [&mut l, &mut r], &[]);
        for i in latency..6000 {
            assert!((l[i] - quiet_in[i - latency]).abs() < 1e-6, "{i}");
        }
    }
}

#[test]
fn effects_limiter_recovers_after_a_peak() {
    let mut device = limiter_with(-6.0, 0.02, 0.002, RATE);
    let mut left = vec![0.1_f32; 48_000];
    left[1000] = 2.0;
    let mut right = left.clone();
    device.process(&mut [&mut left, &mut right], &[]);
    let latency = device.latency_frames() as usize;
    // Well after the release the steady 0.1 is back at unity.
    assert!((left[40_000] - 0.1).abs() < 1e-4, "{}", left[40_000]);
    // The peak itself came out at the ceiling.
    assert!((left[1000 + latency] - 0.501_187).abs() < 1e-3);
}
