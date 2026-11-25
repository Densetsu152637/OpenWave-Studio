//! Time-stretch: exact output length, pitch behaviour and realtime safety of the streaming
//! stretcher. The Rust side of `Stretcher::process` runs under `assert_no_alloc`; allocations
//! made inside the C++ library go through `malloc` and are not visible to that guard, which is
//! why `Stretcher::new` warms the library's buffers up.

use std::f64::consts::TAU;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use ow_dsp::stretch::{Stretcher, stretch_to_length};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const RATE: u32 = 48_000;

fn sine(freq: f64, frames: usize, rate: u32) -> Vec<f32> {
    (0..frames)
        .map(|i| (0.5 * (TAU * freq * i as f64 / f64::from(rate)).sin()) as f32)
        .collect()
}

/// Frequency of a clean sine from rising zero crossings, measured over the middle half.
fn measured_frequency(signal: &[f32], rate: u32) -> f64 {
    let (from, to) = (signal.len() / 4, signal.len() * 3 / 4);
    let mut crossings = Vec::new();
    for i in from.max(1)..to {
        if signal[i - 1] < 0.0 && signal[i] >= 0.0 {
            // Linear interpolation of the crossing position.
            let frac = f64::from(-signal[i - 1]) / f64::from(signal[i] - signal[i - 1]);
            crossings.push(i as f64 - 1.0 + frac);
        }
    }
    assert!(crossings.len() > 4, "not enough crossings");
    let periods = (crossings.len() - 1) as f64;
    periods * f64::from(rate) / (crossings.last().unwrap() - crossings[0])
}

mod stretch {
    use super::*;

    #[test]
    fn stretch_output_length_is_exact() {
        for in_frames in [48_000_usize, 22_050, 100_003] {
            let mono = sine(220.0, in_frames, RATE);
            for ratio in [0.5, 0.6, 0.75, 1.0, 1.3333, 1.5, 2.0] {
                let target = (in_frames as f64 * ratio).round() as usize;
                let out = stretch_to_length(&[&mono], 1, RATE, target, 0.0);
                assert_eq!(out.len(), 1);
                assert_eq!(out[0].len(), target, "{in_frames} frames at ratio {ratio}");
                assert!(out[0].iter().all(|s| s.is_finite()));
            }
        }
    }

    #[test]
    fn stretch_stereo_and_unusual_targets_are_exact() {
        let left = sine(330.0, 30_000, RATE);
        let right = sine(440.0, 30_000, RATE);
        for target in [0, 1, 7, 100, 4_000, 29_999, 60_001] {
            let out = stretch_to_length(&[&left, &right], 2, RATE, target, 0.0);
            assert_eq!(out.len(), 2);
            assert!(out.iter().all(|c| c.len() == target), "target {target}");
            assert!(
                out.iter().flatten().all(|s| s.is_finite()),
                "target {target}"
            );
        }
        // Silence in gives silence out; a missing channel is silent.
        let silent = stretch_to_length(&[&vec![0.0; 20_000]], 1, RATE, 30_000, 0.0);
        assert!(silent[0].iter().all(|s| s.abs() < 1e-6));
        let empty = stretch_to_length(&[], 2, RATE, 100, 0.0);
        assert_eq!(empty, vec![vec![0.0; 100]; 2]);
    }

    #[test]
    fn stretch_keeps_pitch_within_one_percent() {
        let input = sine(440.0, 96_000, RATE);
        for ratio in [0.5, 0.8, 1.25, 2.0] {
            let target = (input.len() as f64 * ratio).round() as usize;
            let out = stretch_to_length(&[&input], 1, RATE, target, 0.0);
            let freq = measured_frequency(&out[0], RATE);
            assert!(
                (freq / 440.0 - 1.0).abs() < 0.01,
                "ratio {ratio}: {freq} Hz"
            );
        }
    }

    #[test]
    fn stretch_pitch_shift_scales_frequency() {
        let input = sine(440.0, 96_000, RATE);
        for semitones in [-12.0_f32, -7.0, 5.0, 12.0] {
            let expected = 440.0 * 2.0_f64.powf(f64::from(semitones) / 12.0);
            for ratio in [1.0, 1.5] {
                let target = (input.len() as f64 * ratio).round() as usize;
                let out = stretch_to_length(&[&input], 1, RATE, target, semitones);
                assert_eq!(out[0].len(), target);
                let freq = measured_frequency(&out[0], RATE);
                assert!(
                    (freq / expected - 1.0).abs() < 0.01,
                    "{semitones} st at ratio {ratio}: {freq} Hz, wanted {expected}"
                );
            }
        }
    }

    #[test]
    fn stretch_streaming_matches_ratio_and_pitch() {
        let input = sine(440.0, 200_000, RATE);
        for ratio in [0.5, 1.0, 2.0] {
            let mut stretcher = Stretcher::new(1, RATE, 256);
            let mut produced = Vec::new();
            let mut position = 0;
            let mut block = vec![0.0_f32; 256];
            while produced.len() < 96_000 {
                let used = stretcher.process(&[&input[position..]], &mut [&mut block[..]], ratio);
                position += used;
                produced.extend_from_slice(&block);
            }
            // Consumed input matches the ratio to within one block.
            let expected = produced.len() as f64 / ratio;
            assert!(
                (position as f64 - expected).abs() <= 1.0,
                "ratio {ratio}: consumed {position}, expected {expected}"
            );
            let tail = &produced[stretcher.latency_frames() + 2_000..];
            let freq = measured_frequency(tail, RATE);
            assert!(
                (freq / 440.0 - 1.0).abs() < 0.01,
                "ratio {ratio}: {freq} Hz"
            );
        }
    }

    #[test]
    fn stretch_streaming_reports_latency() {
        let stretcher = Stretcher::new(2, RATE, 128);
        assert!(stretcher.latency_frames() > 0);
        assert!(stretcher.input_latency_frames() > 0);
        assert_eq!(stretcher.input_frames_for(128, 2.0), 64);
        assert_eq!(stretcher.input_frames_for(128, 0.5), 256);
    }

    #[test]
    fn stretch_process_is_allocation_free() {
        let input_l = sine(330.0, 400_000, RATE);
        let input_r = sine(550.0, 400_000, RATE);
        let mut stretcher = Stretcher::new(2, RATE, 128);
        stretcher.set_pitch_semitones(3.0);
        let mut out_l = vec![0.0_f32; 128];
        let mut out_r = vec![0.0_f32; 128];
        let mut position = 0;
        // Ratios sweep across the whole range, including odd block sizes.
        let ratios = [1.0, 0.25, 4.0, 0.5, 2.0, 1.37, 0.81, 3.3];
        for round in 0..400 {
            let ratio = ratios[round % ratios.len()];
            let frames = if round % 7 == 0 { 77 } else { 128 };
            let used = assert_no_alloc(|| {
                stretcher.process(
                    &[&input_l[position..], &input_r[position..]],
                    &mut [&mut out_l[..frames], &mut out_r[..frames]],
                    ratio,
                )
            });
            position += used;
            assert!(out_l.iter().chain(&out_r).all(|s| s.is_finite()));
        }
        assert_no_alloc(|| stretcher.reset());
        let used = assert_no_alloc(|| {
            stretcher.process(
                &[&input_l[..], &input_r[..]],
                &mut [&mut out_l[..], &mut out_r[..]],
                1.0,
            )
        });
        assert_eq!(used, 128);
    }
}
