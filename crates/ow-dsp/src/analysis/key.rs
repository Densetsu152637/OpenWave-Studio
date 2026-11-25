//! Musical key detection: a chromagram matched against the Krumhansl-Kessler key profiles.
//!
//! The signal is cut into 8192-point (at 44.1 kHz; scaled with the rate) Hann-windowed frames
//! with 50 % overlap. In every frame the spectral peaks between 80 Hz and 2 kHz are located and
//! their frequencies refined by parabolic interpolation on the log magnitude. Each peak adds
//! its magnitude to the pitch class of the nearest equal-tempered semitone (A = 440 Hz) and,
//! with weight 0.6 per step, to the classes of its sub-harmonics f/2 to f/6 (the fundamentals it
//! could be an overtone of), so the overtones of a chord tone support that tone instead of
//! inventing others. The accumulated 12-bin chroma vector is correlated (Pearson) with the 24
//! rotations of the major and minor profiles; the best correlation names the key. A tuning
//! offset is not estimated.

use super::fft::{Fft, hann, next_pow2};

/// Krumhansl-Kessler major profile, starting at the tonic.
const MAJOR_PROFILE: [f64; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
/// Krumhansl-Kessler minor profile, starting at the tonic.
const MINOR_PROFILE: [f64; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];
const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// How many sub-harmonics a spectral peak is credited to (itself included).
const SUBHARMONICS: u32 = 6;
/// Weight multiplier per sub-harmonic step.
const SUBHARMONIC_DECAY: f64 = 0.6;

/// Shortest audio that can be analysed, in seconds.
pub const MIN_KEY_SECONDS: f64 = 1.0;
/// Longest stretch of audio that is analysed, in seconds.
pub const MAX_KEY_SECONDS: f64 = 120.0;

/// Major or minor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Major (Ionian).
    Major,
    /// Natural/harmonic minor (Aeolian).
    Minor,
}

/// A detected key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyEstimate {
    /// Pitch class of the tonic: 0 = C, 1 = C sharp, ... 11 = B.
    pub tonic: u8,
    /// Major or minor.
    pub mode: Mode,
    /// Pearson correlation of the chroma with the winning profile, -1 to 1.
    pub correlation: f64,
    /// 0 to 1: the margin of the winning correlation over the second-best key, scaled so that
    /// a margin of 0.15 or more is 1.
    pub confidence: f64,
}

impl KeyEstimate {
    /// The tonic's name with sharps, such as `F#`.
    pub fn tonic_name(&self) -> &'static str {
        NOTE_NAMES[usize::from(self.tonic % 12)]
    }

    /// Readable form, such as `A minor`.
    pub fn name(&self) -> String {
        let mode = match self.mode {
            Mode::Major => "major",
            Mode::Minor => "minor",
        };
        format!("{} {mode}", self.tonic_name())
    }
}

/// The 12-bin chromagram of `mono` (index 0 is C), or `None` for audio too short or silent.
pub fn chroma(mono: &[f32], sample_rate: u32) -> Option<[f64; 12]> {
    let mono = &*super::finite(mono);
    let rate = f64::from(sample_rate);
    if sample_rate == 0 || (mono.len() as f64) < MIN_KEY_SECONDS * rate {
        return None;
    }
    let used = mono.len().min((MAX_KEY_SECONDS * rate) as usize);
    let size = next_pow2((rate * 0.186).round() as usize).max(1024);
    if used < size {
        return None;
    }
    let hop = size / 2;
    let window = hann(size);
    let fft = Fft::new(size);
    let bin_hz = rate / size as f64;
    let first = ((80.0 / bin_hz).ceil() as usize).max(2);
    let last = ((2_000.0 / bin_hz) as usize).min(size / 2 - 2);
    let (mut re, mut im) = (vec![0.0; size], vec![0.0; size]);
    let mut mag = vec![0.0; size / 2];
    let mut chroma = [0.0; 12];
    let mut start = 0;
    while start + size <= used {
        for i in 0..size {
            re[i] = f64::from(mono[start + i]) * window[i];
            im[i] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        let mut frame_max = 0.0_f64;
        for k in 0..size / 2 {
            mag[k] = re[k].hypot(im[k]);
            frame_max = frame_max.max(mag[k]);
        }
        if frame_max > 1e-6 {
            for k in first..=last {
                // Local maxima above 2 % of the frame's strongest bin.
                if mag[k] < frame_max * 0.02 || mag[k] <= mag[k - 1] || mag[k] < mag[k + 1] {
                    continue;
                }
                let (a, b, c) = (
                    (mag[k - 1] + 1e-12).ln(),
                    (mag[k] + 1e-12).ln(),
                    (mag[k + 1] + 1e-12).ln(),
                );
                let denom = a - 2.0 * b + c;
                let shift = if denom.abs() > 1e-12 {
                    (0.5 * (a - c) / denom).clamp(-0.5, 0.5)
                } else {
                    0.0
                };
                let freq = (k as f64 + shift) * bin_hz;
                let semitones = 12.0 * (freq / 440.0).log2();
                // The peak counts for its own pitch class and, with falling weight, for the
                // classes of the fundamentals it could be a harmonic of (f/2, f/3, ...), so the
                // overtones of a chord tone support that tone instead of inventing others.
                for h in 1..=SUBHARMONICS {
                    let note = semitones - 12.0 * f64::from(h).log2();
                    let class = (note.round() as i64 + 9).rem_euclid(12) as usize;
                    chroma[class] += mag[k] * SUBHARMONIC_DECAY.powi(h as i32 - 1);
                }
            }
        }
        start += hop;
    }
    (chroma.iter().sum::<f64>() > 0.0).then_some(chroma)
}

fn pearson(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let (ma, mb) = (a.iter().sum::<f64>() / 12.0, b.iter().sum::<f64>() / 12.0);
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let (x, y) = (a[i] - ma, b[i] - mb);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    if da <= 0.0 || db <= 0.0 {
        0.0
    } else {
        num / (da * db).sqrt()
    }
}

/// Matches a chroma vector against the 24 key profiles.
pub fn key_from_chroma(chroma: &[f64; 12]) -> Option<KeyEstimate> {
    if chroma.iter().all(|v| *v <= 0.0) {
        return None;
    }
    let mut scored: Vec<(u8, Mode, f64)> = Vec::with_capacity(24);
    for tonic in 0..12u8 {
        for (mode, profile) in [(Mode::Major, &MAJOR_PROFILE), (Mode::Minor, &MINOR_PROFILE)] {
            let rotated: [f64; 12] =
                std::array::from_fn(|i| profile[(i + 12 - usize::from(tonic)) % 12]);
            scored.push((tonic, mode, pearson(chroma, &rotated)));
        }
    }
    scored.sort_by(|a, b| b.2.total_cmp(&a.2));
    let (tonic, mode, correlation) = scored[0];
    let runner_up = scored[1].2;
    Some(KeyEstimate {
        tonic,
        mode,
        correlation,
        confidence: ((correlation - runner_up) / 0.15).clamp(0.0, 1.0),
    })
}

/// Estimates the key of `mono` at `sample_rate`; `None` for audio shorter than
/// [`MIN_KEY_SECONDS`] or silent.
pub fn detect_key(mono: &[f32], sample_rate: u32) -> Option<KeyEstimate> {
    key_from_chroma(&chroma(mono, sample_rate)?)
}
