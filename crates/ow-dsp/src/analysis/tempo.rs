//! Tempo, beat phase and downbeat estimation.
//!
//! 1. [`onset_envelope`]: a band-wise log spectral-flux curve (a short STFT; the magnitudes are
//!    averaged in eight octave-wide bands, log-compressed and differenced, and the positive
//!    rises are summed, so a kick, a snare and a hat count alike; a moving average is
//!    subtracted and the result squared to favour strong onsets), plus a second curve of the
//!    linear low-band (below 150 Hz) rises, which is where kicks and bass accents live.
//! 2. Tempo: the envelope is Hann-windowed and its Fourier magnitude is evaluated on a grid of
//!    candidate tempi from 60 to 200 BPM. A candidate scores the sum of the magnitudes at its
//!    first four harmonics, times a log-Gaussian prior around 110 BPM (1.5 octaves wide) that
//!    settles close calls between octaves. A period-doubling check then moves to half the
//!    tempo while the half tempo's fundamental is at least half as strong as the tempo's own
//!    (a pulse train at P has no energy at P/2). The result is refined on a 0.05 BPM grid.
//! 3. Phase: the envelope is summed along a comb of the estimated period for every half-frame
//!    offset; the best offset is the first beat. The downbeat is the beat position (out of four,
//!    assuming 4/4) whose low-band comb sum is largest.
//!
//! Only the first [`MAX_ANALYSIS_SECONDS`] of a file are analysed. Octave and meter are
//! judgements, not facts: a loop whose only accents are at a different level than the beat can
//! still come out at half or double its nominal tempo.

use std::f64::consts::PI;

use super::fft::{Fft, hann, next_pow2};

/// Lowest reported tempo.
pub const MIN_BPM: f64 = 60.0;
/// Highest reported tempo.
pub const MAX_BPM: f64 = 200.0;
/// Longest stretch of audio that is analysed, in seconds.
pub const MAX_ANALYSIS_SECONDS: f64 = 90.0;
/// Shortest audio that can be analysed, in seconds.
pub const MIN_ANALYSIS_SECONDS: f64 = 2.0;

const HARMONIC_WEIGHTS: [f64; 4] = [1.0, 1.0, 1.0, 1.0];
const PRIOR_CENTRE_BPM: f64 = 110.0;
const PRIOR_SIGMA_OCTAVES: f64 = 1.5;
/// Where inside an analysis window a flux peak places the onset, as a fraction of the window.
/// Upper edges of the analysis bands in hertz.
const BAND_EDGES_HZ: [f64; 7] = [120.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0];
/// The half tempo wins when its fundamental is at least this fraction of the full tempo's.
const HALVING_RATIO: f64 = 0.5;
const ONSET_POSITION: f64 = 0.8;

/// The spectral-flux onset curves of a signal.
#[derive(Debug, Clone)]
pub struct OnsetEnvelope {
    /// Whole-band onset strength, one value per STFT hop.
    pub full: Vec<f64>,
    /// Onset strength below about 150 Hz.
    pub low: Vec<f64>,
    /// Curve values per second.
    pub rate: f64,
    /// Time in seconds of an onset that peaks in curve index 0.
    pub offset_seconds: f64,
}

/// What [`detect_tempo`] found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoEstimate {
    /// Beats per minute, within [`MIN_BPM`] and [`MAX_BPM`].
    pub bpm: f64,
    /// 0 to 1: how clearly the best tempo stands out from the rest of the search range.
    pub confidence: f64,
    /// Time of the first beat in seconds from the start of the audio.
    pub first_beat_seconds: f64,
    /// Time of the first downbeat (the first beat that falls on count one of a 4/4 bar) in
    /// seconds; at most three beats after `first_beat_seconds`.
    pub first_downbeat_seconds: f64,
}

impl TempoEstimate {
    /// Seconds per beat.
    pub fn beat_seconds(&self) -> f64 {
        60.0 / self.bpm
    }
}

/// Computes the onset curves of `mono` at `sample_rate`; empty when the audio is shorter than
/// one analysis window.
pub fn onset_envelope(mono: &[f32], sample_rate: u32) -> OnsetEnvelope {
    let rate = f64::from(sample_rate.max(1));
    let size = next_pow2((rate * 0.0232).round() as usize).max(256);
    let hop = size / 4;
    let mut env = OnsetEnvelope {
        full: Vec::new(),
        low: Vec::new(),
        rate: rate / hop as f64,
        offset_seconds: size as f64 * ONSET_POSITION / rate,
    };
    if mono.len() < size {
        return env;
    }
    let frames = (mono.len() - size) / hop + 1;
    let window = hann(size);
    let fft = Fft::new(size);
    let bin_hz = rate / size as f64;
    let top = ((12_000.0 / bin_hz) as usize).min(size / 2 - 1).max(2);
    let low_top = ((150.0 / bin_hz) as usize).clamp(1, top);
    // Octave-ish bands, so that a kick, a snare and a hat weigh alike in the curve instead of
    // the many high bins drowning the few low ones.
    let mut bands: Vec<(usize, usize)> = Vec::new();
    let mut lo = 1;
    for edge in BAND_EDGES_HZ {
        let hi = ((edge / bin_hz) as usize).min(top);
        if hi >= lo {
            bands.push((lo, hi));
            lo = hi + 1;
        }
    }
    if lo <= top {
        bands.push((lo, top));
    }
    let scale = 100.0 / (size as f64 / 4.0);
    let (mut re, mut im) = (vec![0.0; size], vec![0.0; size]);
    let mut previous = vec![0.0; bands.len()];
    let mut previous_bass = 0.0;
    let (mut full, mut low) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
    for t in 0..frames {
        let start = t * hop;
        for i in 0..size {
            re[i] = f64::from(mono[start + i]) * window[i];
            im[i] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        let mut all = 0.0;
        for (band, &(first, last)) in bands.iter().enumerate() {
            let mean =
                (first..=last).map(|k| re[k].hypot(im[k])).sum::<f64>() / (last - first + 1) as f64;
            let level = (1.0 + scale * mean).ln();
            if t > 0 {
                all += (level - previous[band]).max(0.0);
            }
            previous[band] = level;
        }
        // Linear (not log) magnitudes keep the loudness of accents for the downbeat.
        let bass_now: f64 = (1..=low_top).map(|k| scale * re[k].hypot(im[k])).sum();
        full.push(all);
        low.push(if t > 0 {
            (bass_now - previous_bass).max(0.0)
        } else {
            0.0
        });
        previous_bass = bass_now;
    }
    env.full = detrend(&full, env.rate).iter().map(|v| v * v).collect();
    env.low = detrend(&low, env.rate);
    env
}

/// Subtracts a centred moving average (about 0.4 s) and clips at zero.
fn detrend(values: &[f64], rate: f64) -> Vec<f64> {
    let half = ((0.2 * rate) as usize).max(1);
    let mut prefix = Vec::with_capacity(values.len() + 1);
    prefix.push(0.0);
    for v in values {
        prefix.push(prefix.last().unwrap() + v);
    }
    (0..values.len())
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(values.len()));
            let mean = (prefix[b] - prefix[a]) / (b - a) as f64;
            (values[i] - mean).max(0.0)
        })
        .collect()
}

/// Magnitude of the Hann-windowed DFT of `values` at `freq_hz` (values at `rate` per second).
fn fourier_magnitude(values: &[f64], rate: f64, freq_hz: f64) -> f64 {
    let n = values.len();
    let step = 2.0 * PI * freq_hz / rate;
    let (rot_re, rot_im) = (step.cos(), -step.sin());
    let (mut ph_re, mut ph_im) = (1.0_f64, 0.0_f64);
    let (mut sum_re, mut sum_im) = (0.0, 0.0);
    for (i, v) in values.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos();
        sum_re += v * w * ph_re;
        sum_im += v * w * ph_im;
        let next_re = ph_re * rot_re - ph_im * rot_im;
        ph_im = ph_re * rot_im + ph_im * rot_re;
        ph_re = next_re;
    }
    sum_re.hypot(sum_im)
}

fn prior(bpm: f64) -> f64 {
    let octaves = (bpm / PRIOR_CENTRE_BPM).log2() / PRIOR_SIGMA_OCTAVES;
    (-0.5 * octaves * octaves).exp()
}

fn tempo_score(values: &[f64], rate: f64, bpm: f64) -> f64 {
    let raw: f64 = HARMONIC_WEIGHTS
        .iter()
        .enumerate()
        .map(|(h, w)| w * fourier_magnitude(values, rate, bpm * (h + 1) as f64 / 60.0))
        .sum();
    raw * prior(bpm)
}

/// Linear interpolation of `values` at a fractional `index`; zero outside.
fn sample_at(values: &[f64], index: f64) -> f64 {
    if index < 0.0 {
        return 0.0;
    }
    let i = index as usize;
    if i + 1 >= values.len() {
        return values.get(i).copied().unwrap_or(0.0);
    }
    let f = index - i as f64;
    values[i] * (1.0 - f) + values[i + 1] * f
}

/// Sum of `values` along the comb `offset + (first + k * stride) * period` (curve indices).
fn comb_sum(values: &[f64], offset: f64, period: f64, first: usize, stride: usize) -> f64 {
    let mut sum = 0.0;
    let mut k = 0;
    loop {
        let index = offset + (first + k * stride) as f64 * period;
        if index >= values.len() as f64 {
            return sum;
        }
        sum += sample_at(values, index);
        k += 1;
    }
}

/// Estimates tempo, first beat and first downbeat of `mono` (mixed down to one channel).
///
/// Returns `None` when the audio is shorter than [`MIN_ANALYSIS_SECONDS`], silent, or has no
/// periodic onsets. The tempo is always inside [`MIN_BPM`]..=[`MAX_BPM`]; a pulse train that
/// could be heard in two octaves is placed by the log-Gaussian prior, so a 70 BPM and a 140 BPM
/// rendition of the same loop are told apart only when the loop has accents that show it.
pub fn detect_tempo(mono: &[f32], sample_rate: u32) -> Option<TempoEstimate> {
    let rate = f64::from(sample_rate);
    if sample_rate == 0 || (mono.len() as f64) < MIN_ANALYSIS_SECONDS * rate {
        return None;
    }
    let used = mono.len().min((MAX_ANALYSIS_SECONDS * rate) as usize);
    let env = onset_envelope(&mono[..used], sample_rate);
    if env.full.len() < 8 || env.full.iter().all(|v| *v <= 0.0) {
        return None;
    }

    // Coarse grid, then a fine grid around the best coarse point.
    let coarse: Vec<(f64, f64)> = (0..)
        .map(|i| MIN_BPM + 0.5 * f64::from(i))
        .take_while(|b| *b <= MAX_BPM)
        .map(|b| (b, tempo_score(&env.full, env.rate, b)))
        .collect();
    let best_bpm = coarse
        .iter()
        .copied()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .filter(|(_, s)| *s > 0.0)?
        .0;
    // Period-doubling check: a pulse train at P has no energy at P/2, so when the half tempo
    // is nearly as strong as P itself, the true beat is the slower one.
    let mut centre = best_bpm;
    while centre / 2.0 >= MIN_BPM - 0.5 {
        let strength = |b: f64| {
            (-10..=10)
                .map(|i| {
                    fourier_magnitude(&env.full, env.rate, b * (1.0 + 0.001 * f64::from(i)) / 60.0)
                })
                .fold(0.0, f64::max)
        };
        if strength(centre / 2.0) >= HALVING_RATIO * strength(centre) {
            centre /= 2.0;
        } else {
            break;
        }
    }
    let centre = centre.clamp(MIN_BPM, MAX_BPM);
    let (mut bpm, mut top) = (centre, tempo_score(&env.full, env.rate, centre));
    for i in -20..=20 {
        let b = centre + 0.05 * f64::from(i);
        if !(MIN_BPM..=MAX_BPM).contains(&b) {
            continue;
        }
        let s = tempo_score(&env.full, env.rate, b);
        if s > top {
            (bpm, top) = (b, s);
        }
    }
    let mean = coarse.iter().map(|c| c.1).sum::<f64>() / coarse.len() as f64;
    let confidence = ((top / mean.max(1e-12) - 1.0) / 4.0).clamp(0.0, 1.0);

    // Beat phase: the comb offset (in curve frames, half-frame steps) with the largest sum.
    let period = 60.0 / bpm * env.rate;
    let steps = (period * 2.0).ceil() as usize;
    let phase = (0..steps)
        .map(|s| s as f64 * 0.5)
        .map(|o| (o, comb_sum(&env.full, o, period, 0, 1)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(0.0, |p| p.0);
    // Downbeat: which of four consecutive beats carries the most low-band energy.
    let bass = |b: usize| comb_sum(&env.low, phase, period, b, 4);
    let downbeat = (0..4)
        .max_by(|a, b| bass(*a).total_cmp(&bass(*b)))
        .unwrap_or(0);

    let first_beat = env.offset_seconds + phase / env.rate;
    Some(TempoEstimate {
        bpm,
        confidence,
        first_beat_seconds: first_beat,
        first_downbeat_seconds: first_beat + downbeat as f64 * 60.0 / bpm,
    })
}
