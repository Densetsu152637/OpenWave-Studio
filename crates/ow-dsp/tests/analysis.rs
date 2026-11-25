//! Tempo, beat and key detection on fixtures generated here (drum patterns at several tempi,
//! with swing and a sparse pattern, and synthetic chord progressions), so no audio files are
//! checked in. Run with `--nocapture` to see the per-fixture error table.

use std::f64::consts::TAU;

use ow_dsp::analysis::{Mode, detect_key, detect_tempo, mono_mixdown};

const RATE: u32 = 44_100;
/// Where the first beat of every drum fixture starts, in seconds.
const FIRST_BEAT: f64 = 0.25;

/// Deterministic noise (xorshift), so fixtures are identical on every run and platform.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1_u64 << 52) as f64 - 1.0
    }
}

#[derive(Clone, Copy)]
enum Hit {
    Kick,
    Snare,
    Hat,
}

fn add_hit(buf: &mut [f32], noise: &mut Noise, start: f64, kind: Hit, gain: f64) {
    let first = (start * f64::from(RATE)).round() as usize;
    let len = match kind {
        Hit::Kick => 0.18,
        Hit::Snare => 0.14,
        Hit::Hat => 0.04,
    };
    let mut lowpass = 0.0;
    let mut previous = 0.0;
    for n in 0..(len * f64::from(RATE)) as usize {
        let t = n as f64 / f64::from(RATE);
        let sample = match kind {
            Hit::Kick => {
                // A sine sweeping from 140 to 50 Hz with an exponential decay.
                let phase = TAU * (50.0 * t + 90.0 * 0.03 * (1.0 - (-t / 0.03).exp()));
                phase.sin() * (-t / 0.07).exp()
            }
            Hit::Snare => {
                let white = noise.next();
                lowpass += 0.5 * (white - lowpass);
                (0.7 * white + 0.3 * lowpass + 0.3 * (TAU * 190.0 * t).sin()) * (-t / 0.04).exp()
            }
            Hit::Hat => {
                let white = noise.next();
                let high = white - previous;
                previous = white;
                high * 0.5 * (-t / 0.01).exp()
            }
        };
        if let Some(slot) = buf.get_mut(first + n) {
            *slot += (sample * gain) as f32;
        }
    }
}

/// A drum loop of at least `seconds` seconds. `swing` delays the off-beat eighth notes by that
/// fraction of a beat (1/6 is a triplet feel). A sparse pattern has only kicks, on beats one
/// and three.
fn drum_fixture(bpm: f64, swing: f64, sparse: bool, seconds: f64) -> Vec<f32> {
    let beat = 60.0 / bpm;
    let bars = (seconds / (4.0 * beat)).ceil() as usize;
    let mut buf = vec![0.0_f32; ((bars as f64 * 4.0 * beat + 1.0) * f64::from(RATE)) as usize];
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
    for bar in 0..bars {
        for b in 0..4 {
            let at = FIRST_BEAT + (bar * 4 + b) as f64 * beat;
            match (b, sparse) {
                (0, _) => add_hit(&mut buf, &mut noise, at, Hit::Kick, 0.9),
                (2, _) => add_hit(&mut buf, &mut noise, at, Hit::Kick, 0.6),
                (_, false) => add_hit(&mut buf, &mut noise, at, Hit::Snare, 0.5),
                _ => {}
            }
            if !sparse {
                add_hit(&mut buf, &mut noise, at, Hit::Hat, 0.3);
                add_hit(
                    &mut buf,
                    &mut noise,
                    at + (0.5 + swing) * beat,
                    Hit::Hat,
                    0.2,
                );
            }
        }
    }
    buf
}

fn chord_fixture(
    roots_and_minor: &[(u8, bool)],
    seconds_per_chord: f64,
    repeats: usize,
) -> Vec<f32> {
    let rate = f64::from(RATE);
    let frames_per_chord = (seconds_per_chord * rate) as usize;
    let mut out = Vec::new();
    for _ in 0..repeats {
        for &(root, minor) in roots_and_minor {
            let third = if minor { 3 } else { 4 };
            let notes = [root, root + third, root + 7];
            let mut chord = vec![0.0_f32; frames_per_chord];
            for (n, note) in notes.iter().enumerate() {
                let midi = 48.0 + f64::from(*note) + if n == 0 { -12.0 } else { 0.0 };
                let freq = 440.0 * 2.0_f64.powf((midi - 69.0) / 12.0);
                for (i, sample) in chord.iter_mut().enumerate() {
                    let t = i as f64 / rate;
                    let envelope = (t / 0.02).min(1.0) * (1.0 - (t / seconds_per_chord).powi(8));
                    let tone: f64 = (1..=5)
                        .map(|h| (TAU * freq * f64::from(h) * t).sin() / f64::from(h))
                        .sum();
                    *sample += (0.15 * tone * envelope) as f32;
                }
            }
            out.extend(chord);
        }
    }
    out
}

mod analysis {
    use super::*;

    #[test]
    fn analysis_tempo_within_one_bpm_on_every_fixture() {
        // (bpm, swing in beats, sparse)
        let fixtures = [
            (70.0, 0.0, false),
            (90.0, 0.0, false),
            (100.0, 0.0, false),
            (120.0, 0.0, false),
            (128.0, 0.0, false),
            (140.0, 0.0, false),
            (174.0, 0.0, false),
            (90.0, 1.0 / 6.0, false),
            (120.0, 1.0 / 6.0, false),
            (140.0, 0.1, false),
            (100.0, 0.0, true),
        ];
        let mut failures = Vec::new();
        for (bpm, swing, sparse) in fixtures {
            let audio = drum_fixture(bpm, swing, sparse, 24.0);
            let estimate = detect_tempo(&audio, RATE).expect("tempo");
            let error = estimate.bpm - bpm;
            eprintln!(
                "tempo fixture {bpm:>5} bpm swing {swing:.2} sparse {sparse:5}: found {:.2} (error {error:+.2}), confidence {:.2}, first beat {:.3} s, downbeat {:.3} s",
                estimate.bpm,
                estimate.confidence,
                estimate.first_beat_seconds,
                estimate.first_downbeat_seconds
            );
            if error.abs() >= 1.0 {
                failures.push(format!(
                    "{bpm} bpm swing {swing} sparse {sparse}: {}",
                    estimate.bpm
                ));
            }
        }
        assert!(failures.is_empty(), "off by 1 BPM or more: {failures:?}");
    }

    #[test]
    fn analysis_first_beat_and_downbeat_follow_the_pattern() {
        for bpm in [90.0, 120.0, 140.0] {
            let audio = drum_fixture(bpm, 0.0, false, 24.0);
            let estimate = detect_tempo(&audio, RATE).expect("tempo");
            let beat = 60.0 / bpm;
            assert!(
                (estimate.first_beat_seconds - FIRST_BEAT).abs() < 0.04,
                "{bpm}: first beat {}",
                estimate.first_beat_seconds
            );
            // The kick on count one is the strongest accent: the first downbeat is the first beat.
            assert!(
                (estimate.first_downbeat_seconds - FIRST_BEAT).abs() < 0.04,
                "{bpm}: downbeat {} (beat {beat:.3})",
                estimate.first_downbeat_seconds
            );
        }
    }

    #[test]
    fn analysis_downbeat_skips_an_incomplete_first_bar() {
        // Cut the first two beats off a 120 BPM loop: the first beat heard is count three, so
        // the first downbeat is two beats later.
        let audio = drum_fixture(120.0, 0.0, false, 24.0);
        let cut = ((FIRST_BEAT + 1.0) * f64::from(RATE)) as usize - 5_000;
        let estimate = detect_tempo(&audio[cut..], RATE).expect("tempo");
        let first_hit = FIRST_BEAT + 1.0 - cut as f64 / f64::from(RATE);
        assert!((estimate.first_beat_seconds - first_hit).abs() < 0.04);
        // Count three is the first beat heard; its bar's count one is 2 beats earlier, so the
        // next downbeat is 2 beats later.
        assert!(
            (estimate.first_downbeat_seconds - (first_hit + 1.0)).abs() < 0.04,
            "downbeat {}, first beat {}",
            estimate.first_downbeat_seconds,
            estimate.first_beat_seconds
        );
    }

    #[test]
    fn analysis_tempo_rejects_silence_noise_and_short_audio() {
        assert!(detect_tempo(&vec![0.0; 44_100 * 10], RATE).is_none());
        assert!(detect_tempo(&drum_fixture(120.0, 0.0, false, 24.0)[..44_100], RATE).is_none());
        assert!(detect_tempo(&[], RATE).is_none());
        assert!(detect_tempo(&[0.5; 100], 0).is_none());
    }

    #[test]
    fn analysis_tempo_works_at_other_sample_rates() {
        // The same pattern built at 44.1 kHz and picked by index to 48 kHz and 22.05 kHz.
        let audio = drum_fixture(128.0, 0.0, false, 24.0);
        for rate in [22_050_u32, 48_000, 96_000] {
            let ratio = f64::from(rate) / f64::from(RATE);
            let resampled: Vec<f32> = (0..(audio.len() as f64 * ratio) as usize)
                .map(|i| audio[((i as f64 / ratio) as usize).min(audio.len() - 1)])
                .collect();
            let estimate = detect_tempo(&resampled, rate).expect("tempo");
            assert!(
                (estimate.bpm - 128.0).abs() < 1.0,
                "{rate}: {}",
                estimate.bpm
            );
        }
    }

    #[test]
    fn analysis_key_is_correct_on_chord_fixtures() {
        // (tonic pitch class, minor, progression as (root, minor) chords)
        let fixtures = [
            (
                "C major",
                0,
                Mode::Major,
                vec![(0, false), (5, false), (7, false), (0, false)],
            ),
            (
                "A minor",
                9,
                Mode::Minor,
                vec![(9, true), (2, true), (4, false), (9, true)],
            ),
            (
                "D major",
                2,
                Mode::Major,
                vec![(2, false), (7, false), (9, false), (2, false)],
            ),
            (
                "F# minor",
                6,
                Mode::Minor,
                vec![(6, true), (11, true), (1, false), (6, true)],
            ),
            (
                "Eb major",
                3,
                Mode::Major,
                vec![(3, false), (8, false), (10, false), (3, false)],
            ),
            (
                "G minor",
                7,
                Mode::Minor,
                vec![(7, true), (0, true), (2, false), (7, true)],
            ),
        ];
        for (name, tonic, mode, progression) in fixtures {
            let audio = chord_fixture(&progression, 2.0, 2);
            let key = detect_key(&audio, RATE).expect("key");
            eprintln!(
                "key fixture {name}: found {} (r {:.2}, confidence {:.2})",
                key.name(),
                key.correlation,
                key.confidence
            );
            assert_eq!(
                (key.tonic, key.mode),
                (tonic, mode),
                "{name}: {}",
                key.name()
            );
            assert!(key.confidence > 0.0);
        }
    }

    #[test]
    fn analysis_key_rejects_silence_and_short_audio() {
        assert!(detect_key(&vec![0.0; 44_100 * 5], RATE).is_none());
        assert!(detect_key(&[0.3; 1000], RATE).is_none());
    }

    #[test]
    fn analysis_mono_mixdown_averages_channels() {
        let mixed = mono_mixdown(&[&[1.0, 0.0, 0.5], &[0.0, 1.0]]);
        assert_eq!(mixed, vec![0.5, 0.5, 0.25]);
        assert!(mono_mixdown(&[]).is_empty());
    }

    #[test]
    fn analysis_survives_non_finite_samples() {
        let mut drums = drum_fixture(120.0, 0.0, false, 24.0);
        let mut chords = chord_fixture(&[(0, false), (5, false), (7, false), (0, false)], 2.0, 2);
        for buf in [&mut drums, &mut chords] {
            for i in (0..buf.len()).step_by(1009) {
                buf[i] = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY][i % 3];
            }
        }
        let tempo = detect_tempo(&drums, RATE).expect("tempo despite bad samples");
        assert!(tempo.bpm.is_finite() && tempo.first_beat_seconds.is_finite());
        let key = detect_key(&chords, RATE).expect("key despite bad samples");
        assert!(key.correlation.is_finite());
        // All bad: silence, so nothing to report.
        assert!(detect_tempo(&vec![f32::NAN; 44_100 * 5], RATE).is_none());
        assert!(detect_key(&vec![f32::INFINITY; 44_100 * 5], RATE).is_none());
    }
}
