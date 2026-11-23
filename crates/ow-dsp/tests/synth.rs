//! The built-in synth: pitch, envelope, event timing, voices, aliasing and realtime safety.

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use ow_dsp::instruments::synth::{
    self, PARAM_ATTACK, PARAM_CUTOFF, PARAM_DECAY, PARAM_GAIN, PARAM_RELEASE, PARAM_RESONANCE,
    PARAM_SUSTAIN, PARAM_WAVEFORM, Synth, VOICES,
};
use ow_dsp::{Device, Event, find};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const RATE: f32 = 48_000.0;

/// A synth with a flat, fast patch: sine, filter wide open, no resonance, sustain at full.
fn flat(rate: f32, waveform: f64) -> Synth {
    let mut synth = Synth::new();
    synth.prepare(rate, 4096);
    synth.set_param(PARAM_WAVEFORM, waveform);
    synth.set_param(PARAM_CUTOFF, 20_000.0);
    synth.set_param(PARAM_RESONANCE, 0.0);
    synth.set_param(PARAM_ATTACK, 0.001);
    synth.set_param(PARAM_DECAY, 0.001);
    synth.set_param(PARAM_SUSTAIN, 1.0);
    synth.set_param(PARAM_RELEASE, 0.05);
    synth.set_param(PARAM_GAIN, 0.0);
    synth
}

/// Renders `frames` frames in one call; returns the left channel (the right must equal it).
fn render(synth: &mut Synth, events: &[Event], frames: usize) -> Vec<f32> {
    let mut left = vec![1.0; frames];
    let mut right = vec![1.0; frames];
    synth.process(&mut [&mut left, &mut right], events);
    assert_eq!(left, right, "the synth is dual mono");
    left
}

/// Frequency from positive-going zero crossings, interpolated linearly.
fn measure_hz(signal: &[f32], rate: f32) -> f64 {
    let mut crossings = Vec::new();
    for (i, pair) in signal.windows(2).enumerate() {
        if pair[0] <= 0.0 && pair[1] > 0.0 {
            let fraction = f64::from(-pair[0]) / f64::from(pair[1] - pair[0]);
            crossings.push(i as f64 + fraction);
        }
    }
    assert!(crossings.len() > 10, "too few cycles: {}", crossings.len());
    let cycles = (crossings.len() - 1) as f64;
    cycles * f64::from(rate) / (crossings.last().unwrap() - crossings[0])
}

fn cents(measured: f64, expected: f64) -> f64 {
    1200.0 * (measured / expected).log2()
}

#[test]
fn synth_note_renders_the_expected_pitch_within_a_cent() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        for key in [24_u8, 48, 60, 69, 81, 96, 108] {
            let mut synth = flat(rate, 2.0);
            let out = render(&mut synth, &[Event::note_on(0, key, 100)], rate as usize);
            let expected = 440.0 * 2.0_f64.powf((f64::from(key) - 69.0) / 12.0);
            let tail = &out[rate as usize / 10..];
            let error = cents(measure_hz(tail, rate), expected);
            assert!(error.abs() < 1.0, "key {key} at {rate}: {error} cents");
        }
    }
}

#[test]
fn synth_saw_and_square_keep_their_pitch_through_the_filter() {
    for waveform in [0.0, 1.0] {
        let mut synth = flat(RATE, waveform);
        synth.set_param(PARAM_CUTOFF, 6_000.0);
        let out = render(&mut synth, &[Event::note_on(0, 57, 100)], 48_000);
        let error = cents(measure_hz(&out[4800..], RATE), 220.0);
        assert!(error.abs() < 1.0, "waveform {waveform}: {error} cents");
    }
}

#[test]
fn synth_is_silent_after_the_release_and_its_tail() {
    let mut synth = flat(RATE, 0.0);
    synth.set_param(PARAM_RELEASE, 0.05);
    let release = 4800; // note-off after 0.1 s
    let out = render(
        &mut synth,
        &[Event::note_on(0, 60, 127), Event::note_off(release, 60)],
        48_000,
    );
    assert!(
        out[..release as usize].iter().any(|s| s.abs() > 0.1),
        "sounds while held"
    );
    // The release is a 60 dB fall in 0.05 s; 100 dB is reached after 0.0833 s. Allow 0.1 s.
    let silent_from = release as usize + 4800;
    assert!(
        out[silent_from..].iter().all(|s| *s == 0.0),
        "exact silence after the tail"
    );
    assert_eq!(synth.active_voices(), 0);
    // And it really did fade rather than cut: a tenth into the tail is far quieter than held.
    let held = out[..release as usize]
        .iter()
        .fold(0.0_f32, |m, s| m.max(s.abs()));
    let mid_tail = out[release as usize + 2400..silent_from]
        .iter()
        .fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!(mid_tail < held * 0.1, "{mid_tail} vs {held}");
}

#[test]
fn synth_note_on_lands_on_its_sample_offset() {
    for waveform in [0.0, 1.0, 2.0] {
        let mut synth = flat(RATE, waveform);
        let out = render(&mut synth, &[Event::note_on(37, 69, 100)], 256);
        assert!(
            out[..37].iter().all(|s| *s == 0.0),
            "silent before the offset"
        );
        assert!(
            out[37] != 0.0,
            "sounds on the offset itself (waveform {waveform})"
        );
    }
    // A note-off at an offset starts the release there: the level falls from that sample.
    let mut synth = flat(RATE, 1.0);
    synth.set_param(PARAM_RELEASE, 0.01);
    let out = render(
        &mut synth,
        &[Event::note_on(0, 69, 100), Event::note_off(1000, 69)],
        4000,
    );
    let before = out[900..1000].iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    let after = out[2000..2100].iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!(before > 0.5 && after < before * 0.01, "{before} {after}");
}

#[test]
fn synth_velocity_scales_the_level() {
    let peak = |velocity| {
        let mut synth = flat(RATE, 2.0);
        let out = render(&mut synth, &[Event::note_on(0, 60, velocity)], 4800);
        out.iter().fold(0.0_f32, |m, s| m.max(s.abs()))
    };
    assert!((peak(127) / peak(64) - 127.0 / 64.0).abs() < 0.02);
}

#[test]
fn synth_gain_parameter_is_in_decibels() {
    let peak = |gain| {
        let mut synth = flat(RATE, 2.0);
        synth.set_param(PARAM_GAIN, gain);
        let out = render(&mut synth, &[Event::note_on(0, 60, 127)], 4800);
        out.iter().fold(0.0_f32, |m, s| m.max(s.abs()))
    };
    assert!((peak(0.0) - 1.0).abs() < 0.01);
    assert!((peak(-6.0206) - 0.5).abs() < 0.01);
}

#[test]
fn synth_plays_sixteen_voices_and_steals_beyond() {
    let mut synth = flat(RATE, 2.0);
    let held: Vec<Event> = (0..VOICES as u8)
        .map(|i| Event::note_on(0, 40 + i, 100))
        .collect();
    let out = render(&mut synth, &held, 2048);
    assert_eq!(synth.active_voices(), VOICES);
    assert!(out.iter().all(|s| s.is_finite()));
    // A seventeenth note steals a voice rather than adding one.
    let out = render(&mut synth, &[Event::note_on(0, 90, 100)], 2048);
    assert_eq!(synth.active_voices(), VOICES);
    assert!(out.iter().all(|s| s.is_finite()));
    // All keys released: everything fades out.
    let offs: Vec<Event> = (0..VOICES as u8)
        .map(|i| Event::note_off(0, 40 + i))
        .chain(std::iter::once(Event::note_off(0, 90)))
        .collect();
    render(&mut synth, &offs, 48_000);
    assert_eq!(synth.active_voices(), 0);
}

#[test]
fn synth_note_off_releases_every_voice_on_the_key_and_reset_silences() {
    let mut synth = flat(RATE, 2.0);
    render(
        &mut synth,
        &[Event::note_on(0, 60, 100), Event::note_on(10, 60, 100)],
        256,
    );
    assert_eq!(synth.active_voices(), 2);
    render(&mut synth, &[Event::note_off(0, 60)], 48_000);
    assert_eq!(synth.active_voices(), 0);
    render(&mut synth, &[Event::note_on(0, 60, 100)], 64);
    synth.reset();
    assert!(render(&mut synth, &[], 64).iter().all(|s| *s == 0.0));
}

#[test]
fn synth_envelope_shapes_attack_decay_sustain() {
    let mut synth = flat(RATE, 2.0);
    synth.set_param(PARAM_ATTACK, 0.1);
    synth.set_param(PARAM_DECAY, 0.05);
    synth.set_param(PARAM_SUSTAIN, 0.5);
    // A 49 Hz sine: the envelope is its peak over a window of a few cycles.
    let out = render(&mut synth, &[Event::note_on(0, 31, 127)], 48_000);
    let peak = |from: usize, to: usize| out[from..to].iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!(peak(0, 480) < 0.15, "attack starts quiet: {}", peak(0, 480));
    assert!(peak(4400, 4900) > 0.85, "attack reaches full level");
    assert!((peak(30_000, 48_000) - 0.5).abs() < 0.08, "sustains at 0.5");
}

#[test]
fn synth_band_limited_saw_aliases_less_than_a_naive_saw() {
    // Key 96 is 2093 Hz: its 12th harmonic (25 116 Hz) folds to 22 884 Hz, where a band-limited
    // saw should have almost nothing.
    let key = 96;
    let f0 = 440.0 * 2.0_f64.powf((f64::from(key) - 69.0) / 12.0);
    let mut synth = flat(RATE, 0.0);
    let frames = 48_000;
    let out = render(&mut synth, &[Event::note_on(0, key, 127)], frames);
    let band_limited = &out[4800..];
    let naive: Vec<f32> = (4800..frames)
        .map(|i| {
            let phase = (f0 * i as f64 / f64::from(RATE)).fract();
            (2.0 * phase - 1.0) as f32
        })
        .collect();
    let alias_hz = f64::from(RATE) - 12.0 * f0;
    let level = |signal: &[f32], hz: f64| {
        // Goertzel magnitude, normalised by length.
        let w = std::f64::consts::TAU * hz / f64::from(RATE);
        let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
        for x in signal {
            let s = f64::from(*x) + 2.0 * w.cos() * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2).sqrt() / signal.len() as f64
    };
    let fundamental = level(band_limited, f0);
    let alias = level(band_limited, alias_hz);
    let naive_alias = level(&naive, alias_hz) / level(&naive, f0);
    let ratio = alias / fundamental;
    assert!(
        ratio < naive_alias * 0.25,
        "alias/fundamental {ratio} vs naive {naive_alias}"
    );
    assert!(ratio < 0.01, "{ratio}");
}

#[test]
fn synth_output_stays_finite_at_extreme_settings() {
    let mut synth = Synth::new();
    synth.prepare(RATE, 512);
    synth.set_param(PARAM_RESONANCE, 1.0);
    synth.set_param(PARAM_CUTOFF, 20.0);
    synth.set_param(PARAM_GAIN, 6.0);
    synth.set_param(PARAM_CUTOFF, f64::NAN);
    synth.set_param(999, 1.0);
    let events: Vec<Event> = (0..16)
        .map(|i| Event::note_on(i, 20 + i as u8 * 7, 127))
        .collect();
    let out = render(&mut synth, &events, 480);
    assert!(out.iter().all(|s| s.is_finite()));
    synth.set_param(PARAM_CUTOFF, 20_000.0);
    let out = render(&mut synth, &[], 4800);
    assert!(out.iter().all(|s| s.is_finite() && s.abs() < 100.0));
}

#[test]
fn synth_descriptors_are_consistent_and_registered() {
    let info = find(synth::KIND).expect("registered");
    assert!(info.instrument);
    assert_eq!(info.params.len(), 8);
    let mut ids: Vec<u32> = info.params.iter().map(|p| p.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), info.params.len(), "ids are unique");
    for p in info.params {
        assert!(p.min < p.max, "{}", p.name);
        assert!((p.min..=p.max).contains(&p.default), "{}", p.name);
        if p.scale == ow_dsp::Scale::Log {
            assert!(p.min > 0.0, "{}", p.name);
        }
        assert_eq!(p.clamp(f64::NAN), p.default);
        assert_eq!(p.clamp(p.max * 2.0 + 1.0), p.max);
    }
    // A device built from the info plays at its defaults.
    let mut device = info.create(RATE, 256);
    let mut left = [0.0_f32; 256];
    let mut right = [0.0_f32; 256];
    device.process(&mut [&mut left, &mut right], &[Event::note_on(0, 60, 100)]);
    assert!(left.iter().any(|s| *s != 0.0));
    assert!(find("builtin.nothing").is_none());
}

#[test]
fn synth_is_realtime_safe() {
    let mut synth = Synth::new();
    synth.prepare(RATE, 128);
    let mut left = [0.0_f32; 128];
    let mut right = [0.0_f32; 128];
    // Events drive note-ons (with steals past 16 voices), note-offs and parameter changes.
    for block in 0..2000_u32 {
        let events = [
            Event::note_on(3, (30 + block % 60) as u8, 100),
            Event::note_on(20, (35 + block % 50) as u8, 80),
            Event::note_off(64, (30 + (block + 7) % 60) as u8),
            Event::note_off(100, (35 + (block + 3) % 50) as u8),
        ];
        assert_no_alloc(|| {
            if block % 50 == 0 {
                synth.set_param(PARAM_CUTOFF, 200.0 + f64::from(block));
                synth.set_param(PARAM_WAVEFORM, f64::from(block % 3));
            }
            if block % 500 == 499 {
                synth.reset();
            }
            synth.process(&mut [&mut left, &mut right], &events);
        });
        assert!(left.iter().all(|s| s.is_finite()));
    }
}
