//! Decode and resample the fixtures (`tests/fixtures`, generated from a fixed signal: 8820
//! frames at 44.1 kHz, left 0.5 sin(440 Hz), right 0.25 sin(880 Hz)).
//!
//! Tolerances: lossless formats (WAV, AIFF, FLAC; 16-bit sources) match the signal to 1e-3
//! sample by sample; lossy formats (MP3, Ogg) are checked by tone amplitude (within 10 %) and
//! length (within 2200 frames, the codec delay and padding). Resampling to 48 kHz keeps the
//! tone amplitude within 3 % and the length within 2 frames of the exact ratio.

use std::f64::consts::TAU;
use std::fs;
use std::path::{Path, PathBuf};

use ow_media::import::{ImportError, ImportLimits, decode_file, import_file, resample};

const RATE: u32 = 44_100;
const FRAMES: usize = 8_820;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Amplitude of the sine at `hz` in `x` (single-bin DFT, valid for whole periods and a long
/// enough window).
fn amplitude(x: &[f32], rate: u32, hz: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (i, s) in x.iter().enumerate() {
        let phase = TAU * hz * i as f64 / f64::from(rate);
        re += f64::from(*s) * phase.cos();
        im += f64::from(*s) * phase.sin();
    }
    2.0 * (re * re + im * im).sqrt() / x.len() as f64
}

fn check_tone(left: &[f32], right: &[f32], rate: u32, tolerance: f64) {
    // Skip the edges (codec delay and ramps, resampler ramp) when measuring.
    let (l, r) = (
        &left[3000..left.len() - 3000],
        &right[3000..right.len() - 3000],
    );
    let al = amplitude(l, rate, 440.0);
    let ar = amplitude(r, rate, 880.0);
    assert!((al - 0.5).abs() < 0.5 * tolerance, "left amplitude {al}");
    assert!((ar - 0.25).abs() < 0.25 * tolerance, "right amplitude {ar}");
}

#[test]
fn lossless_formats_decode_exactly() {
    for name in ["tone.wav", "tone.aiff", "tone.flac"] {
        let pcm = decode_file(&fixture(name), &ImportLimits::default()).expect(name);
        assert_eq!(pcm.sample_rate, RATE, "{name}");
        assert_eq!(pcm.channels.len(), 2, "{name}");
        assert_eq!(pcm.frames(), FRAMES, "{name}");
        for i in (0..FRAMES).step_by(7) {
            let t = i as f64 / f64::from(RATE);
            let l = 0.5 * (TAU * 440.0 * t).sin();
            let r = 0.25 * (TAU * 880.0 * t).sin();
            assert!(
                (f64::from(pcm.channels[0][i]) - l).abs() < 1e-3,
                "{name} left {i}"
            );
            assert!(
                (f64::from(pcm.channels[1][i]) - r).abs() < 1e-3,
                "{name} right {i}"
            );
        }
    }
}

#[test]
fn lossy_formats_decode_to_the_same_tone() {
    for name in ["tone.mp3", "tone.ogg"] {
        let pcm = decode_file(&fixture(name), &ImportLimits::default()).expect(name);
        assert_eq!(pcm.sample_rate, RATE, "{name}");
        assert_eq!(pcm.channels.len(), 2, "{name}");
        assert!(
            pcm.frames().abs_diff(FRAMES) <= 2200,
            "{name}: {} frames",
            pcm.frames()
        );
        check_tone(&pcm.channels[0], &pcm.channels[1], RATE, 0.1);
    }
}

#[test]
fn every_format_resamples_to_the_project_rate() {
    for name in ["tone.wav", "tone.aiff", "tone.flac", "tone.mp3", "tone.ogg"] {
        let pcm = decode_file(&fixture(name), &ImportLimits::default()).expect(name);
        let out = resample(&pcm, 48_000).expect(name);
        assert_eq!(out.sample_rate, 48_000);
        let expected = (pcm.frames() as f64 * 48_000.0 / 44_100.0).ceil() as usize;
        assert!(
            out.frames().abs_diff(expected) <= 2,
            "{name}: {} vs {expected}",
            out.frames()
        );
        check_tone(&out.channels[0], &out.channels[1], 48_000, 0.1);
    }
    // The lossless path is held to the tighter bound.
    let pcm = decode_file(&fixture("tone.wav"), &ImportLimits::default()).unwrap();
    let out = resample(&pcm, 48_000).unwrap();
    check_tone(&out.channels[0], &out.channels[1], 48_000, 0.03);
}

#[test]
fn import_copies_under_a_content_hash_and_deduplicates() {
    let bundle = tempfile::tempdir().unwrap();
    let limits = ImportLimits::default();
    let first = import_file(&fixture("tone.flac"), bundle.path(), 48_000, &limits).unwrap();
    assert!(!first.deduplicated);
    assert_eq!(first.sample_rate, 48_000);
    assert_eq!(first.channels, 2);
    assert!(first.path.starts_with("audio/") && first.path.ends_with(".wav"));
    assert!(first.hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(bundle.path().join(&first.path).is_file());

    // Same file again, even under another name: nothing new is written.
    let copy = bundle.path().join("renamed.flac");
    fs::copy(fixture("tone.flac"), &copy).unwrap();
    let again = import_file(&copy, bundle.path(), 48_000, &limits).unwrap();
    assert!(again.deduplicated);
    assert_eq!(
        (again.hash, again.frames),
        (first.hash.clone(), first.frames)
    );
    assert_eq!(
        fs::read_dir(bundle.path().join("audio")).unwrap().count(),
        1
    );

    // Another project rate is different content.
    let other = import_file(&fixture("tone.flac"), bundle.path(), 44_100, &limits).unwrap();
    assert_ne!(other.hash, first.hash);
    assert_eq!(other.frames as usize, FRAMES);
}

#[test]
fn hostile_files_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let limits = ImportLimits::default();
    // Garbage, empty and truncated files.
    let junk = dir.path().join("junk.wav");
    fs::write(&junk, vec![0xA5_u8; 4096]).unwrap();
    assert!(decode_file(&junk, &limits).is_err());
    let empty = dir.path().join("empty.mp3");
    fs::write(&empty, b"").unwrap();
    assert!(decode_file(&empty, &limits).is_err());
    // Every format cut short, and every format with flipped bytes, never panics.
    for name in ["tone.wav", "tone.aiff", "tone.flac", "tone.mp3", "tone.ogg"] {
        let bytes = fs::read(fixture(name)).unwrap();
        let ext = name.rsplit('.').next().unwrap();
        for cut in [10, 100, bytes.len() / 2] {
            let path = dir.path().join(format!("cut.{ext}"));
            fs::write(&path, &bytes[..cut.min(bytes.len())]).unwrap();
            let _ = decode_file(&path, &limits);
        }
        for step in [3_usize, 17, 101] {
            let mut bad = bytes.clone();
            for i in (step..bad.len()).step_by(step * 11) {
                bad[i] ^= 0xFF;
            }
            let path = dir.path().join(format!("flip.{ext}"));
            fs::write(&path, &bad).unwrap();
            let _ = decode_file(&path, &limits);
        }
    }
    // Bounds: file size, decoded length and channel count.
    let tiny = ImportLimits {
        max_file_bytes: 1000,
        ..limits
    };
    assert!(matches!(
        decode_file(&fixture("tone.wav"), &tiny),
        Err(ImportError::TooLarge(_))
    ));
    let short = ImportLimits {
        max_samples: 1000,
        ..limits
    };
    assert!(matches!(
        decode_file(&fixture("tone.wav"), &short),
        Err(ImportError::TooLarge(_))
    ));
    let mono_only = ImportLimits {
        max_channels: 1,
        ..limits
    };
    assert!(matches!(
        decode_file(&fixture("tone.wav"), &mono_only),
        Err(ImportError::TooLarge(_))
    ));
    // A WAV header claiming a huge body over a tiny file stays bounded.
    let mut lie = fs::read(fixture("tone.wav")).unwrap();
    lie[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    let path = dir.path().join("lie.wav");
    fs::write(&path, &lie).unwrap();
    let _ = decode_file(&path, &limits);
}
