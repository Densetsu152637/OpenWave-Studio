//! Property tests for the timeline math: conversion round trips, ramp integrals against numeric
//! integration, and bar/beat structure.

use proptest::prelude::*;

use super::*;

const RATES: [u32; 5] = [22_050, 44_100, 48_000, 96_000, 192_000];

/// A valid tempo map: 1 to 6 points, gaps of 0.25 to 64 beats, 20 to 400 bpm, random ramps.
fn tempo_map() -> impl Strategy<Value = TempoMap> {
    proptest::collection::vec((0.25_f64..64.0, 20.0_f64..400.0, any::<bool>()), 1..6).prop_map(
        |raw| {
            let mut beat = 0.0;
            let points = raw
                .into_iter()
                .map(|(gap, bpm, ramp)| {
                    let point = TempoPoint { beat, bpm, ramp };
                    beat += gap;
                    point
                })
                .collect();
            TempoMap::new(points).expect("generated points are valid")
        },
    )
}

fn signature() -> impl Strategy<Value = TimeSignature> {
    (1_u32..=16, 0_u32..=5).prop_map(|(numerator, power)| {
        TimeSignature::new(numerator, 1 << power).expect("valid signature")
    })
}

/// A valid signature map: 1 to 6 entries at increasing bars.
fn signature_map() -> impl Strategy<Value = TimeSignatureMap> {
    proptest::collection::vec((1_u32..8, signature()), 0..5).prop_flat_map(|rest| {
        signature().prop_map(move |first| {
            let mut bar = 0;
            let mut changes = vec![(0, first)];
            for (gap, sig) in &rest {
                bar += gap;
                changes.push((bar, *sig));
            }
            TimeSignatureMap::new(changes).expect("generated changes are valid")
        })
    })
}

/// Composite Simpson integral of `f` over `[a, b]` with `n` (even) intervals.
fn simpson(f: impl Fn(f64) -> f64, a: f64, b: f64, n: usize) -> f64 {
    let h = (b - a) / n as f64;
    let mut sum = f(a) + f(b);
    for i in 1..n {
        sum += f(a + h * i as f64) * if i % 2 == 1 { 4.0 } else { 2.0 };
    }
    sum * h / 3.0
}

proptest! {
    #[test]
    fn time_beats_seconds_round_trip(map in tempo_map(), beat in 0.0_f64..400.0) {
        let seconds = map.beats_to_seconds(beat);
        let back = map.seconds_to_beats(seconds);
        prop_assert!((back - beat).abs() <= 1e-9 * beat.max(1.0), "{beat} -> {seconds} -> {back}");
    }

    #[test]
    fn time_seconds_beats_round_trip(map in tempo_map(), seconds in 0.0_f64..400.0) {
        let beat = map.seconds_to_beats(seconds);
        let back = map.beats_to_seconds(beat);
        prop_assert!((back - seconds).abs() <= 1e-9 * seconds.max(1.0), "{seconds} -> {beat} -> {back}");
    }

    #[test]
    fn time_beat_sample_round_trip(
        map in tempo_map(),
        beat in 0.0_f64..400.0,
        rate in proptest::sample::select(RATES.to_vec()),
    ) {
        let samples = map.beats_to_samples(beat, rate);
        let back = map.samples_to_beats(samples, rate);
        prop_assert!((back - beat).abs() <= 1e-9 * beat.max(1.0));
        // A whole sample maps to a beat and back within a millionth of a sample.
        let whole = sample_index(samples);
        let again = map.beats_to_samples(map.samples_to_beats(whole as f64, rate), rate);
        prop_assert!((again - whole as f64).abs() < 1e-5, "{whole} -> {again}");
    }

    #[test]
    fn time_conversion_is_strictly_increasing(
        map in tempo_map(),
        a in 0.0_f64..400.0,
        step in 1e-6_f64..10.0,
    ) {
        prop_assert!(map.beats_to_seconds(a + step) > map.beats_to_seconds(a));
    }

    #[test]
    fn time_constant_tempo_is_linear(bpm in BPM_MIN..BPM_MAX, beat in 0.0_f64..1000.0) {
        let map = TempoMap::constant(bpm).unwrap();
        let expected = beat * 60.0 / bpm;
        prop_assert!((map.beats_to_seconds(beat) - expected).abs() <= 1e-12 * expected.max(1.0));
        prop_assert_eq!(map.bpm_at(beat), bpm);
    }

    #[test]
    fn time_ramp_closed_form_matches_numeric_integral(
        start in 20.0_f64..400.0,
        end in 20.0_f64..400.0,
        len in 0.25_f64..64.0,
        fraction in 0.0_f64..1.0,
    ) {
        let map = TempoMap::new(vec![
            TempoPoint::ramp(0.0, start),
            TempoPoint::constant(len, end),
        ]).unwrap();
        // The time a beat range takes is the integral of 60 / bpm over it.
        let upto = len * fraction;
        let numeric = simpson(|b| 60.0 / map.bpm_at(b), 0.0, upto, 2000);
        let closed = map.beats_to_seconds(upto);
        prop_assert!((numeric - closed).abs() <= 1e-9 * closed.max(1.0), "{numeric} vs {closed}");
        // The whole ramp: 60 L / (end - start) * ln(end / start).
        let whole = if (end - start).abs() > 1e-9 {
            60.0 * len / (end - start) * (end / start).ln()
        } else {
            60.0 * len / start
        };
        prop_assert!((map.beats_to_seconds(len) - whole).abs() <= 1e-9 * whole.max(1.0));
        // The tempo reaches the target exactly at the next point.
        prop_assert_eq!(map.bpm_at(len), end);
    }

    #[test]
    fn time_ramp_tempo_is_linear_in_beats(
        start in 20.0_f64..400.0,
        end in 20.0_f64..400.0,
        len in 0.25_f64..64.0,
        fraction in 0.0_f64..1.0,
    ) {
        let map = TempoMap::new(vec![
            TempoPoint::ramp(0.0, start),
            TempoPoint::constant(len, end),
        ]).unwrap();
        let expected = start + (end - start) * fraction;
        prop_assert!((map.bpm_at(len * fraction) - expected).abs() <= 1e-9 * expected);
    }

    #[test]
    fn time_signature_bar_beat_tick_round_trip(
        map in signature_map(),
        bar in 0_u32..60,
        beat_pick in 0_u32..64,
        tick_pick in 0_u32..4000,
    ) {
        let signature = map.signature_at_bar(bar);
        let position = BarBeatTick {
            bar,
            beat: beat_pick % signature.numerator,
            tick: tick_pick % signature.ticks_per_beat() as u32,
        };
        let beats = map.beats_of(position);
        prop_assert_eq!(map.bar_beat_tick(beats), position);
    }

    #[test]
    fn time_signature_bars_tile_the_timeline(map in signature_map(), bar in 0_u32..80) {
        let start = map.bar_start_beat(bar);
        let next = map.bar_start_beat(bar + 1);
        let length = map.signature_at_bar(bar).bar_length();
        prop_assert!(((next - start) - length).abs() < 1e-9);
        let position = map.bar_beat_tick(start);
        prop_assert_eq!(position, BarBeatTick { bar, beat: 0, tick: 0 });
        // Just before the next bar line is the last tick of this bar.
        let last = map.bar_beat_tick(next - 1.0 / f64::from(PPQ));
        prop_assert_eq!(last.bar, bar);
    }

    #[test]
    fn time_signature_grid_walks_every_beat(map in signature_map(), from in 0.0_f64..120.0) {
        let mut grid = map.grid_at_or_after(from);
        prop_assert!(grid.beat >= from - 1e-6);
        prop_assert!(grid.beat - from <= 4.0 + 1e-6);
        for _ in 0..60 {
            let position = map.bar_beat_tick(grid.beat);
            prop_assert_eq!(position, BarBeatTick { bar: grid.bar, beat: grid.index, tick: 0 });
            let next = map.next_grid(grid);
            let unit = map.signature_at_beat(grid.beat).beat_length();
            prop_assert!(((next.beat - grid.beat) - unit).abs() < 1e-9, "{grid:?} -> {next:?}");
            grid = next;
        }
    }
}

#[test]
fn time_tempo_map_rejects_bad_input() {
    assert_eq!(TempoMap::new(vec![]), Err(TempoError::Empty));
    assert_eq!(
        TempoMap::new(vec![TempoPoint::constant(1.0, 120.0)]),
        Err(TempoError::FirstNotAtZero)
    );
    assert_eq!(
        TempoMap::new(vec![
            TempoPoint::constant(0.0, 120.0),
            TempoPoint::constant(0.0, 130.0)
        ]),
        Err(TempoError::NotIncreasing(1))
    );
    assert_eq!(
        TempoMap::new(vec![TempoPoint::constant(0.0, f64::NAN)]),
        Err(TempoError::BadBpm(0))
    );
    assert_eq!(TempoMap::constant(5.0), Err(TempoError::BadBpm(0)));
    assert_eq!(TempoMap::constant(1000.0), Err(TempoError::BadBpm(0)));
}

#[test]
fn time_known_values() {
    // 120 bpm: half a second per beat, 24000 samples per beat at 48 kHz.
    let map = TempoMap::default();
    assert_eq!(map.beats_to_seconds(4.0), 2.0);
    assert_eq!(map.beats_to_samples(1.0, 48_000), 24_000.0);
    assert_eq!(beats_to_sample_index(&map, 0.5, 48_000), 12_000);
    assert_eq!(map.seconds_to_beats(3.0), 6.0);
    // Before the start the first tempo continues (count-in).
    assert_eq!(map.beats_to_seconds(-2.0), -1.0);
    // A ramp from 60 to 120 over 4 beats takes 60 * 4 / 60 * ln(2) seconds.
    let ramp = TempoMap::new(vec![
        TempoPoint::ramp(0.0, 60.0),
        TempoPoint::constant(4.0, 120.0),
    ])
    .unwrap();
    let expected = 4.0 * std::f64::consts::LN_2;
    assert!((ramp.beats_to_seconds(4.0) - expected).abs() < 1e-12);
    // After the ramp the tempo holds at 120 bpm.
    assert!((ramp.beats_to_seconds(6.0) - expected - 1.0).abs() < 1e-12);
}

#[test]
fn time_ticks_and_sample_index() {
    assert_eq!(beats_to_ticks(1.0), 960);
    assert_eq!(beats_to_ticks(0.25), 240);
    assert_eq!(ticks_to_beats(1920), 2.0);
    assert_eq!(sample_index(-3.0), 0);
    assert_eq!(sample_index(f64::NAN), 0);
    assert_eq!(sample_index(2.5), 3);
    assert_eq!(sample_index(2.4999), 2);
}

#[test]
fn time_signature_known_values() {
    let map = TimeSignatureMap::new(vec![
        (0, TimeSignature::new(4, 4).unwrap()),
        (2, TimeSignature::new(6, 8).unwrap()),
        (4, TimeSignature::new(3, 4).unwrap()),
    ])
    .unwrap();
    assert_eq!(map.bar_start_beat(1), 4.0);
    assert_eq!(map.bar_start_beat(2), 8.0);
    // 6/8 bars are 3 quarter notes long.
    assert_eq!(map.bar_start_beat(3), 11.0);
    assert_eq!(map.bar_start_beat(4), 14.0);
    assert_eq!(map.bar_start_beat(5), 17.0);
    assert_eq!(
        map.bar_beat_tick(9.5),
        BarBeatTick {
            bar: 2,
            beat: 3,
            tick: 0
        }
    );
    let grid = map.grid_at_or_after(8.25);
    assert_eq!((grid.beat, grid.bar, grid.index), (8.5, 2, 1));
    let grid = map.grid_at_or_after(13.75);
    assert_eq!((grid.beat, grid.bar, grid.index), (14.0, 4, 0));
    assert_eq!(map.grid_at_or_after(0.0).beat, 0.0);
}

#[test]
fn time_signature_rejects_bad_input() {
    assert_eq!(TimeSignature::new(0, 4), Err(SignatureError::BadNumerator));
    assert_eq!(
        TimeSignature::new(4, 3),
        Err(SignatureError::BadDenominator)
    );
    assert_eq!(
        TimeSignature::new(4, 128),
        Err(SignatureError::BadDenominator)
    );
    assert_eq!(TimeSignatureMap::new(vec![]), Err(SignatureError::Empty));
    assert_eq!(
        TimeSignatureMap::new(vec![(1, TimeSignature::COMMON)]),
        Err(SignatureError::FirstNotAtBarZero)
    );
    assert_eq!(
        TimeSignatureMap::new(vec![(0, TimeSignature::COMMON), (0, TimeSignature::COMMON)]),
        Err(SignatureError::NotIncreasing(1))
    );
}
