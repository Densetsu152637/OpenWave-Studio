//! Offline analysis of a rendered stereo mix: loudness (ITU-R BS.1770-4 / EBU R 128), true peak,
//! sample peak, RMS, crest factor, stereo correlation and band energies; plus
//! [`normalize`], which scales a buffer to a target loudness under a true-peak ceiling.
//!
//! Everything works on interleaved stereo `f32` (`[l0, r0, l1, r1, ...]`) at any sample rate and
//! allocates freely: it is for control threads and offline tools, never the audio thread. No
//! third-party DSP code is used; the filters, the 4x oversampler and the FFT are written here.
//!
//! # Loudness
//!
//! Each channel runs through the two-stage K-weighting filter (a high shelf modelling the head,
//! then the 38 Hz "RLB" high-pass), with coefficients derived from the standard's analogue
//! prototypes for the actual sample rate ([`k_weighting`]). Mean squares are taken over 100 ms
//! segments; a 400 ms *momentary* block is four segments and a 3 s *short-term* block thirty, so
//! blocks overlap by 75 % and 97 %. A block's loudness is `-0.691 + 10 log10(sum of channel mean
//! squares)` (both channels weigh 1.0). Integrated loudness gates the 400 ms blocks twice: blocks
//! at or below -70 LUFS are dropped, then blocks more than 10 LU below the mean of the rest.
//! Audio shorter than a window uses the whole of what there is as one window; a trailing partial
//! segment (under 100 ms) is ignored.
//!
//! # True peak
//!
//! 4x oversampling with a 192-tap Kaiser-windowed sinc, split into four 48-tap polyphase
//! branches ([`true_peak`]). The branch for phase 0 reproduces the input samples exactly.
//!
//! # Band energies
//!
//! A Welch estimate (Hann window, 4096 points, 50 % overlap, both channels averaged) summed into
//! eight bands ([`BAND_EDGES_HZ`]). Each value is the band's share of the mean square in dB, so a
//! full-scale sine inside a band reads about -3 dB and the bands add up to the total mean square.

use std::f64::consts::PI;

/// Lower edges of the eight analysis bands in hertz; the last band runs to the Nyquist frequency.
pub const BAND_EDGES_HZ: [f64; 8] = [0.0, 60.0, 150.0, 400.0, 1_000.0, 2_500.0, 6_000.0, 12_000.0];

/// Floor of the reported loudness and levels: anything quieter is `None` (digital silence).
const SILENCE_DB: f64 = -200.0;
/// Absolute gate of the integrated loudness measurement, in LUFS.
const ABSOLUTE_GATE: f64 = -70.0;
/// Relative gate of the integrated loudness measurement, in LU below the ungated mean.
const RELATIVE_GATE: f64 = -10.0;
/// Offset of the loudness scale (BS.1770-4 equation 2).
const LOUDNESS_OFFSET: f64 = -0.691;
/// Frames of the FFT used for the band energies.
const FFT_SIZE: usize = 4096;

/// What [`analyze`] measures. A level that is `None` is digital silence (below -200 dB).
#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    /// Sample rate of the analysed audio.
    pub sample_rate: u32,
    /// Frames (per channel) analysed.
    pub frames: u64,
    /// Gated integrated loudness in LUFS; `None` when no block passes the gates.
    pub integrated_lufs: Option<f64>,
    /// Highest 3 s short-term loudness in LUFS.
    pub short_term_max_lufs: Option<f64>,
    /// Highest 400 ms momentary loudness in LUFS.
    pub momentary_max_lufs: Option<f64>,
    /// True peak (4x oversampled, over both channels) in dBTP.
    pub true_peak_dbtp: Option<f64>,
    /// Highest absolute sample value in dBFS.
    pub sample_peak_db: Option<f64>,
    /// RMS over both channels in dBFS.
    pub rms_db: Option<f64>,
    /// Sample peak minus RMS, in dB (the crest factor).
    pub crest_factor_db: Option<f64>,
    /// Pearson correlation of left and right, -1 to 1; 0 when either channel is silent.
    pub correlation: f64,
    /// Energy of the eight bands of [`BAND_EDGES_HZ`], in dB re full-scale mean square; `None`
    /// for an empty band.
    pub band_energies_db: [Option<f64>; 8],
}

/// Measures interleaved stereo `samples` at `sample_rate`.
pub fn analyze(samples: &[f32], sample_rate: u32) -> Analysis {
    let frames = samples.len() / 2;
    let (mut sum_l, mut sum_r, mut sum_lr) = (0.0_f64, 0.0_f64, 0.0_f64);
    let mut peak = 0.0_f32;
    for pair in samples.as_chunks::<2>().0 {
        let (l, r) = (f64::from(pair[0]), f64::from(pair[1]));
        sum_l += l * l;
        sum_r += r * r;
        sum_lr += l * r;
        peak = peak.max(pair[0].abs()).max(pair[1].abs());
    }
    let mean_square = if frames == 0 {
        0.0
    } else {
        (sum_l + sum_r) / (2.0 * frames as f64)
    };
    let sample_peak_db = amplitude_db(f64::from(peak));
    let rms_db = power_db(mean_square);
    let correlation = if sum_l > 0.0 && sum_r > 0.0 {
        (sum_lr / (sum_l * sum_r).sqrt()).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    let loudness = Loudness::measure(samples, sample_rate);
    Analysis {
        sample_rate,
        frames: frames as u64,
        integrated_lufs: loudness.integrated,
        short_term_max_lufs: loudness.short_term_max,
        momentary_max_lufs: loudness.momentary_max,
        true_peak_dbtp: amplitude_db(f64::from(true_peak(samples))),
        sample_peak_db,
        rms_db,
        crest_factor_db: sample_peak_db.zip(rms_db).map(|(p, r)| p - r),
        correlation,
        band_energies_db: band_energies(samples, sample_rate).map(power_db),
    }
}

fn amplitude_db(amplitude: f64) -> Option<f64> {
    power_db(amplitude * amplitude)
}

fn power_db(power: f64) -> Option<f64> {
    let db = 10.0 * power.log10();
    (power > 0.0 && db > SILENCE_DB).then_some(db)
}

// ---------------------------------------------------------------------------------------------
// K-weighting and loudness
// ---------------------------------------------------------------------------------------------

/// One biquad section in transposed direct form II: `b` numerator, `a1` and `a2` denominator
/// (`a0` is 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Biquad {
    /// Numerator coefficients `b0, b1, b2`.
    pub b: [f64; 3],
    /// Denominator coefficients `a1, a2`.
    pub a: [f64; 2],
}

impl Biquad {
    fn process(&self, state: &mut [f64; 2], x: f64) -> f64 {
        let y = self.b[0] * x + state[0];
        state[0] = self.b[1] * x - self.a[0] * y + state[1];
        state[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The two K-weighting sections of BS.1770-4 for `sample_rate`: the pre-filter (high shelf) and
/// the RLB high-pass. The standard lists 48 kHz coefficients only; these are the bilinear
/// transforms of the same analogue prototypes, so any rate gets the same response shape.
pub fn k_weighting(sample_rate: u32) -> [Biquad; 2] {
    let fs = f64::from(sample_rate.max(1));
    // Stage 1: high shelf, +4 dB above about 1.7 kHz.
    let (f0, gain_db, q) = (
        1_681.974_450_955_533,
        3.999_843_853_973_347,
        0.707_175_236_955_419_6,
    );
    let k = (PI * f0 / fs).tan();
    let vh = 10.0_f64.powf(gain_db / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    // Stage 2: second-order high-pass at 38 Hz.
    let (f0, q) = (38.135_470_876_024_44, 0.500_327_037_323_877_3);
    let k = (PI * f0 / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    let high_pass = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    [shelf, high_pass]
}

/// Frames in a 100 ms segment.
fn segment_frames(sample_rate: u32) -> usize {
    ((f64::from(sample_rate) / 10.0).round() as usize).max(1)
}

/// Loudness in LUFS of a summed channel mean square (`None` for silence).
fn lufs(energy: f64) -> Option<f64> {
    let value = LOUDNESS_OFFSET + 10.0 * energy.log10();
    (energy > 0.0 && value > SILENCE_DB).then_some(value)
}

/// The loudness figures of [`analyze`].
struct Loudness {
    integrated: Option<f64>,
    short_term_max: Option<f64>,
    momentary_max: Option<f64>,
}

impl Loudness {
    fn measure(samples: &[f32], sample_rate: u32) -> Self {
        let segments = segment_energies(samples, sample_rate);
        let momentary = windows(&segments, 4);
        Self {
            integrated: gated_loudness(&momentary),
            short_term_max: max_loudness(&windows(&segments, 30)),
            momentary_max: max_loudness(&momentary),
        }
    }
}

/// Per 100 ms segment: the sum over both channels of the K-weighted mean square.
fn segment_energies(samples: &[f32], sample_rate: u32) -> Vec<f64> {
    let [shelf, high_pass] = k_weighting(sample_rate);
    let length = segment_frames(sample_rate);
    let frames = samples.len() / 2;
    let mut energies = Vec::with_capacity(frames / length);
    let mut states = [[[0.0_f64; 2]; 2]; 2];
    let mut sums = [0.0_f64; 2];
    let mut in_segment = 0;
    for pair in samples.as_chunks::<2>().0 {
        for channel in 0..2 {
            let x = f64::from(pair[channel]);
            let x = if x.is_finite() { x } else { 0.0 };
            let shelved = shelf.process(&mut states[channel][0], x);
            let y = high_pass.process(&mut states[channel][1], shelved);
            sums[channel] += y * y;
        }
        in_segment += 1;
        if in_segment == length {
            energies.push((sums[0] + sums[1]) / length as f64);
            sums = [0.0; 2];
            in_segment = 0;
        }
    }
    energies
}

/// Mean energy of every run of `size` consecutive segments (one per segment step); fewer
/// segments than `size` give the mean of all of them as the only window.
fn windows(segments: &[f64], size: usize) -> Vec<f64> {
    if segments.is_empty() {
        return Vec::new();
    }
    let size = size.min(segments.len());
    let mut sum: f64 = segments[..size].iter().sum();
    let mut out = vec![sum / size as f64];
    for index in size..segments.len() {
        sum += segments[index] - segments[index - size];
        // Guards against the running sum drifting below zero in silence.
        out.push(sum.max(0.0) / size as f64);
    }
    out
}

fn max_loudness(energies: &[f64]) -> Option<f64> {
    energies
        .iter()
        .copied()
        .fold(None, |best: Option<f64>, e| {
            Some(best.map_or(e, |b| b.max(e)))
        })
        .and_then(lufs)
}

/// The two-stage gate of BS.1770-4 over 400 ms block energies.
fn gated_loudness(blocks: &[f64]) -> Option<f64> {
    let absolute: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|e| lufs(*e).is_some_and(|l| l > ABSOLUTE_GATE))
        .collect();
    if absolute.is_empty() {
        return None;
    }
    let mean = absolute.iter().sum::<f64>() / absolute.len() as f64;
    let threshold = lufs(mean)? + RELATIVE_GATE;
    let relative: Vec<f64> = absolute
        .into_iter()
        .filter(|e| lufs(*e).is_some_and(|l| l > threshold))
        .collect();
    if relative.is_empty() {
        return None;
    }
    lufs(relative.iter().sum::<f64>() / relative.len() as f64)
}

/// Gated integrated loudness of interleaved stereo `samples` in LUFS; `None` for silence.
pub fn integrated_loudness(samples: &[f32], sample_rate: u32) -> Option<f64> {
    gated_loudness(&windows(&segment_energies(samples, sample_rate), 4))
}

// ---------------------------------------------------------------------------------------------
// True peak
// ---------------------------------------------------------------------------------------------

const OVERSAMPLE: usize = 4;
const TAPS_PER_PHASE: usize = 48;

/// Zeroth-order modified Bessel function of the first kind (series), for the Kaiser window.
fn bessel_i0(x: f64) -> f64 {
    let (mut term, mut sum) = (1.0_f64, 1.0_f64);
    let quarter = x * x / 4.0;
    for k in 1..60 {
        term *= quarter / (f64::from(k) * f64::from(k));
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// The polyphase branches: `phases[p][k]` multiplies the input `k` frames back (k = 0 is the
/// newest of the 48 frames the branch spans). Each branch sums to 1 (unity gain at DC), and
/// branch 0 is a pure delay of [`TAPS_PER_PHASE`]` / 2` frames.
fn interpolation_filter() -> [[f64; TAPS_PER_PHASE]; OVERSAMPLE] {
    const LENGTH: usize = OVERSAMPLE * TAPS_PER_PHASE;
    const BETA: f64 = 9.0;
    let centre = (LENGTH / 2) as f64;
    let norm = bessel_i0(BETA);
    let mut prototype = [0.0_f64; LENGTH];
    for (n, tap) in prototype.iter_mut().enumerate() {
        let t = (n as f64 - centre) / OVERSAMPLE as f64;
        let sinc = if t == 0.0 {
            1.0
        } else {
            (PI * t).sin() / (PI * t)
        };
        let r = (n as f64 - centre) / centre;
        let window = bessel_i0(BETA * (1.0 - r * r).max(0.0).sqrt()) / norm;
        *tap = sinc * window;
    }
    let mut phases = [[0.0; TAPS_PER_PHASE]; OVERSAMPLE];
    for (p, phase) in phases.iter_mut().enumerate() {
        let sum: f64 = (0..TAPS_PER_PHASE)
            .map(|k| prototype[k * OVERSAMPLE + p])
            .sum();
        for (k, tap) in phase.iter_mut().enumerate() {
            // The prototype's tap k * 4 + p belongs to the input that many output frames back.
            *tap = prototype[k * OVERSAMPLE + p] / sum;
        }
    }
    phases
}

/// True peak of interleaved stereo `samples` as a linear amplitude: the highest magnitude of
/// either channel after 4x oversampling (never below the sample peak).
pub fn true_peak(samples: &[f32]) -> f32 {
    let phases = interpolation_filter();
    // The largest sum of absolute taps bounds how far a branch can exceed its inputs' maximum;
    // stretches of audio that cannot beat the peak found so far are skipped.
    let bound = phases
        .iter()
        .map(|phase| phase.iter().map(|t| t.abs()).sum::<f64>())
        .fold(1.0_f64, f64::max);
    let frames = samples.len() / 2;
    let mut peak = samples
        .iter()
        .fold(0.0_f64, |m, s| m.max(f64::from(s.abs())));
    const CHUNK: usize = 256;
    let mut window = vec![0.0_f64; TAPS_PER_PHASE + CHUNK];
    for channel in 0..2 {
        let mut start = 0;
        while start < frames {
            let end = (start + CHUNK).min(frames);
            // Input frames `start - 47 ..= end - 1` feed the outputs for frames `start..end`
            // (the filter's own delay of 24 frames is skipped by looking 24 frames further on).
            let first =
                start as isize - (TAPS_PER_PHASE as isize - 1) + (TAPS_PER_PHASE / 2) as isize;
            let count = TAPS_PER_PHASE + CHUNK - 1;
            let mut top = 0.0_f64;
            for (i, slot) in window.iter_mut().take(count).enumerate() {
                let index = first + i as isize;
                *slot = if (0..frames as isize).contains(&index) {
                    f64::from(samples[index as usize * 2 + channel])
                } else {
                    0.0
                };
                top = top.max(slot.abs());
            }
            if top * bound > peak {
                for t in 0..(end - start) {
                    // `window[t + 47]` is the newest frame of this output frame's span.
                    let newest = t + TAPS_PER_PHASE - 1;
                    for phase in &phases[1..] {
                        let value: f64 = phase
                            .iter()
                            .enumerate()
                            .map(|(k, tap)| tap * window[newest - k])
                            .sum();
                        peak = peak.max(value.abs());
                    }
                }
            }
            start = end;
        }
    }
    peak as f32
}

// ---------------------------------------------------------------------------------------------
// Band energies
// ---------------------------------------------------------------------------------------------

/// In-place radix-2 FFT of `re`/`im` (length a power of two).
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut size = 2;
    while size <= n {
        let angle = -2.0 * PI / size as f64;
        let (wr, wi) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(size) {
            let (mut cr, mut ci) = (1.0_f64, 0.0_f64);
            for k in 0..size / 2 {
                let (a, b) = (start + k, start + k + size / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let next = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next;
            }
        }
        size <<= 1;
    }
}

/// Mean-square power in each band of [`BAND_EDGES_HZ`] (linear), by Welch's method.
fn band_energies(samples: &[f32], sample_rate: u32) -> [f64; 8] {
    let frames = samples.len() / 2;
    let window: Vec<f64> = (0..FFT_SIZE)
        .map(|n| 0.5 - 0.5 * (2.0 * PI * n as f64 / FFT_SIZE as f64).cos())
        .collect();
    let window_power: f64 = window.iter().map(|w| w * w).sum();
    let hop = FFT_SIZE / 2;
    let mut starts: Vec<usize> = (0..)
        .map(|i| i * hop)
        .take_while(|s| s + FFT_SIZE <= frames)
        .collect();
    if starts.is_empty() && frames > 0 {
        starts.push(0); // zero-padded single frame
    }
    let bin_hz = f64::from(sample_rate) / FFT_SIZE as f64;
    let band_of = |bin: usize| {
        let hz = bin as f64 * bin_hz;
        BAND_EDGES_HZ
            .iter()
            .rposition(|edge| hz >= *edge)
            .unwrap_or(0)
    };
    let mut bands = [0.0_f64; 8];
    let (mut re, mut im) = (vec![0.0; FFT_SIZE], vec![0.0; FFT_SIZE]);
    for channel in 0..2 {
        for &start in &starts {
            for n in 0..FFT_SIZE {
                let frame = start + n;
                let x = if frame < frames {
                    f64::from(samples[frame * 2 + channel])
                } else {
                    0.0
                };
                re[n] = if x.is_finite() { x * window[n] } else { 0.0 };
                im[n] = 0.0;
            }
            fft(&mut re, &mut im);
            for bin in 0..=FFT_SIZE / 2 {
                let power = re[bin] * re[bin] + im[bin] * im[bin];
                // One-sided spectrum: every bin but DC and Nyquist stands for two.
                let share = if bin == 0 || bin == FFT_SIZE / 2 {
                    1.0
                } else {
                    2.0
                };
                bands[band_of(bin)] += share * power / (FFT_SIZE as f64 * window_power);
            }
        }
    }
    let averages = (starts.len() * 2).max(1) as f64;
    bands.map(|b| b / averages)
}

// ---------------------------------------------------------------------------------------------
// Normalisation
// ---------------------------------------------------------------------------------------------

/// What [`normalize`] did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Normalization {
    /// Gain applied, in dB (0 when the buffer is silent).
    pub gain_db: f64,
    /// True when the gain was lowered to keep the true peak under the ceiling, so the result is
    /// quieter than the target.
    pub limited_by_ceiling: bool,
    /// Integrated loudness before, in LUFS.
    pub loudness_before: Option<f64>,
    /// Integrated loudness after, in LUFS.
    pub loudness_after: Option<f64>,
    /// True peak after, in dBTP.
    pub true_peak_after: Option<f64>,
}

/// Scales interleaved stereo `samples` so their integrated loudness is `target_lufs`, lowering
/// the gain further when the true peak would exceed `ceiling_dbtp`. A plain gain is applied (no
/// limiting or clipping), so the loudness before the ceiling bites is exact; a silent buffer is
/// left alone. Returns what was applied and the measured result.
pub fn normalize(
    samples: &mut [f32],
    sample_rate: u32,
    target_lufs: f64,
    ceiling_dbtp: f64,
) -> Normalization {
    let loudness_before = integrated_loudness(samples, sample_rate);
    let peak_before = amplitude_db(f64::from(true_peak(samples)));
    let (Some(loudness), Some(peak)) = (loudness_before, peak_before) else {
        return Normalization {
            gain_db: 0.0,
            limited_by_ceiling: false,
            loudness_before,
            loudness_after: loudness_before,
            true_peak_after: peak_before,
        };
    };
    let wanted = target_lufs - loudness;
    // A linear gain moves the true peak by the same number of decibels.
    let allowed = ceiling_dbtp - peak;
    let limited_by_ceiling = allowed < wanted;
    let gain_db = wanted.min(allowed);
    let gain = 10.0_f64.powf(gain_db / 20.0);
    for sample in samples.iter_mut() {
        *sample = (f64::from(*sample) * gain) as f32;
    }
    Normalization {
        gain_db,
        limited_by_ceiling,
        loudness_before,
        loudness_after: integrated_loudness(samples, sample_rate),
        true_peak_after: amplitude_db(f64::from(true_peak(samples))),
    }
}
