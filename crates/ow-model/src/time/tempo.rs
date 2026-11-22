//! The tempo map: constant and linear-ramp tempo segments and beat/second/sample conversion.

use std::fmt;

/// Slowest tempo a map accepts, in quarter notes per minute.
pub const BPM_MIN: f64 = 10.0;
/// Fastest tempo a map accepts.
pub const BPM_MAX: f64 = 999.0;

/// A tempo change at a beat position.
///
/// The tempo is `bpm` from `beat` on. With `ramp` set the tempo moves linearly (in beats) to
/// the tempo of the next point, reaching it exactly there; otherwise it holds until the next
/// point. `ramp` on the last point has no effect: the tempo holds forever.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoPoint {
    /// Position of the change, in beats.
    pub beat: f64,
    /// Tempo at that position, in quarter notes per minute.
    pub bpm: f64,
    /// Whether the tempo ramps linearly to the next point.
    pub ramp: bool,
}

impl TempoPoint {
    /// A tempo that holds from `beat`.
    pub fn constant(beat: f64, bpm: f64) -> Self {
        Self {
            beat,
            bpm,
            ramp: false,
        }
    }

    /// A tempo that ramps linearly from `bpm` at `beat` to the next point's tempo.
    pub fn ramp(beat: f64, bpm: f64) -> Self {
        Self {
            beat,
            bpm,
            ramp: true,
        }
    }
}

/// Why a tempo map was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempoError {
    /// No points.
    Empty,
    /// The first point is not at beat 0.
    FirstNotAtZero,
    /// Point `n` is not after the previous one (or its beat is not finite).
    NotIncreasing(usize),
    /// Point `n` has a tempo outside [`BPM_MIN`]..=[`BPM_MAX`] (or not finite).
    BadBpm(usize),
}

impl fmt::Display for TempoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a tempo map needs at least one point"),
            Self::FirstNotAtZero => f.write_str("the first tempo point must be at beat 0"),
            Self::NotIncreasing(n) => write!(f, "tempo point {n} is not after the previous one"),
            Self::BadBpm(n) => write!(
                f,
                "tempo point {n} is outside {BPM_MIN}..={BPM_MAX} beats per minute"
            ),
        }
    }
}

impl std::error::Error for TempoError {}

/// Tempo over the timeline: constant and linear-ramp segments.
///
/// Immutable once built. Seconds elapsed at every point are precomputed, so converting a beat
/// to time (and back) is a binary search plus one closed-form evaluation: no allocation, no
/// iteration, safe to call on the audio thread.
///
/// A ramp is linear in *beats*: over a segment of `L` beats from `b0` to `b1` beats per minute,
/// `bpm(x) = b0 + (b1 - b0) * x / L`, and the time it takes is the integral of `60 / bpm`,
/// which is `60 L / (b1 - b0) * ln(b1 / b0)` over the whole segment. Positions before beat 0
/// (count-in) continue at the first tempo.
#[derive(Debug, Clone, PartialEq)]
pub struct TempoMap {
    points: Box<[TempoPoint]>,
    /// Seconds elapsed at each point's beat.
    seconds: Box<[f64]>,
}

impl Default for TempoMap {
    /// 120 beats per minute.
    fn default() -> Self {
        Self::from_points(vec![TempoPoint::constant(0.0, 120.0)])
    }
}

impl TempoMap {
    /// A map with one constant tempo.
    pub fn constant(bpm: f64) -> Result<Self, TempoError> {
        Self::new(vec![TempoPoint::constant(0.0, bpm)])
    }

    /// Builds a map from points ordered by beat; the first must be at beat 0.
    pub fn new(points: Vec<TempoPoint>) -> Result<Self, TempoError> {
        let first = points.first().ok_or(TempoError::Empty)?;
        if first.beat != 0.0 {
            return Err(TempoError::FirstNotAtZero);
        }
        for (index, point) in points.iter().enumerate() {
            if !(BPM_MIN..=BPM_MAX).contains(&point.bpm) {
                return Err(TempoError::BadBpm(index));
            }
            if !point.beat.is_finite() || (index > 0 && point.beat <= points[index - 1].beat) {
                return Err(TempoError::NotIncreasing(index));
            }
        }
        Ok(Self::from_points(points))
    }

    fn from_points(points: Vec<TempoPoint>) -> Self {
        let mut seconds = Vec::with_capacity(points.len());
        let mut elapsed = 0.0;
        seconds.push(elapsed);
        for pair in points.windows(2) {
            let len = pair[1].beat - pair[0].beat;
            elapsed += seconds_into_segment(&pair[0], pair[1].bpm, len, len);
            seconds.push(elapsed);
        }
        Self {
            points: points.into_boxed_slice(),
            seconds: seconds.into_boxed_slice(),
        }
    }

    /// The tempo points, ordered by beat.
    pub fn points(&self) -> &[TempoPoint] {
        &self.points
    }

    /// Index of the segment containing `beat` (the last point at or before it).
    fn segment_at_beat(&self, beat: f64) -> usize {
        self.points
            .partition_point(|point| point.beat <= beat)
            .saturating_sub(1)
    }

    /// The tempo at `beat`, in beats per minute.
    pub fn bpm_at(&self, beat: f64) -> f64 {
        let index = self.segment_at_beat(beat);
        let point = &self.points[index];
        match self.points.get(index + 1) {
            Some(next) if point.ramp => {
                let fraction = ((beat - point.beat) / (next.beat - point.beat)).clamp(0.0, 1.0);
                point.bpm + (next.bpm - point.bpm) * fraction
            }
            _ => point.bpm,
        }
    }

    /// Seconds from beat 0 to `beat`.
    pub fn beats_to_seconds(&self, beat: f64) -> f64 {
        if beat < 0.0 {
            return beat * 60.0 / self.points[0].bpm;
        }
        let index = self.segment_at_beat(beat);
        let point = &self.points[index];
        let offset = beat - point.beat;
        let elapsed = match self.points.get(index + 1) {
            Some(next) => seconds_into_segment(point, next.bpm, next.beat - point.beat, offset),
            None => offset * 60.0 / point.bpm,
        };
        self.seconds[index] + elapsed
    }

    /// The beat reached `seconds` after beat 0. Inverse of
    /// [`beats_to_seconds`](Self::beats_to_seconds).
    pub fn seconds_to_beats(&self, seconds: f64) -> f64 {
        if seconds < 0.0 {
            return seconds * self.points[0].bpm / 60.0;
        }
        let index = self
            .seconds
            .partition_point(|&at| at <= seconds)
            .saturating_sub(1);
        let point = &self.points[index];
        let into = seconds - self.seconds[index];
        let offset = match self.points.get(index + 1) {
            Some(next) => beats_into_segment(point, next.bpm, next.beat - point.beat, into),
            None => into * point.bpm / 60.0,
        };
        point.beat + offset
    }

    /// Fractional sample position of `beat` at `sample_rate`.
    pub fn beats_to_samples(&self, beat: f64, sample_rate: u32) -> f64 {
        self.beats_to_seconds(beat) * f64::from(sample_rate)
    }

    /// The beat at fractional sample position `samples`.
    pub fn samples_to_beats(&self, samples: f64, sample_rate: u32) -> f64 {
        self.seconds_to_beats(samples / f64::from(sample_rate.max(1)))
    }
}

/// Seconds for the first `offset` beats of the segment of `len` beats starting at `point` and
/// ending at tempo `end_bpm`.
fn seconds_into_segment(point: &TempoPoint, end_bpm: f64, len: f64, offset: f64) -> f64 {
    let delta = end_bpm - point.bpm;
    if point.ramp && delta != 0.0 {
        // Integral of 60 / bpm(x) with bpm(x) = b0 + delta * x / len.
        60.0 * len / delta * (delta * (offset / len) / point.bpm).ln_1p()
    } else {
        offset * 60.0 / point.bpm
    }
}

/// Beats covered by the first `seconds` of a segment; inverse of [`seconds_into_segment`].
fn beats_into_segment(point: &TempoPoint, end_bpm: f64, len: f64, seconds: f64) -> f64 {
    let delta = end_bpm - point.bpm;
    if point.ramp && delta != 0.0 {
        let exponent = seconds * delta / (60.0 * len);
        (len * point.bpm / delta * exponent.exp_m1()).clamp(0.0, len)
    } else {
        (seconds * point.bpm / 60.0).clamp(0.0, len)
    }
}
