//! The time-signature map: bar and beat structure over the timeline.

use std::fmt;

use super::PPQ;

/// Largest numerator accepted.
const MAX_NUMERATOR: u32 = 64;
/// Largest denominator accepted.
const MAX_DENOMINATOR: u32 = 64;
/// Slack, in ticks, when splitting a beat position into whole ticks (guards `f64` noise).
const TICK_SLACK: f64 = 1e-6;
/// Slack, in grid units, when finding the next grid point at or after a beat.
const GRID_SLACK: f64 = 1e-9;

/// A time signature such as 3/4 or 6/8. The denominator is a power of two and names the note
/// value that gets one beat; a *beat* elsewhere in the model is always a quarter note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSignature {
    /// Beats per bar, `1..=64`.
    pub numerator: u32,
    /// Note value per beat: 1, 2, 4, 8, 16, 32 or 64.
    pub denominator: u32,
}

impl TimeSignature {
    /// 4/4.
    pub const COMMON: Self = Self {
        numerator: 4,
        denominator: 4,
    };

    /// A validated signature.
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, SignatureError> {
        if !(1..=MAX_NUMERATOR).contains(&numerator) {
            return Err(SignatureError::BadNumerator);
        }
        if !(1..=MAX_DENOMINATOR).contains(&denominator) || !denominator.is_power_of_two() {
            return Err(SignatureError::BadDenominator);
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    /// Length of one signature beat in quarter-note beats (4/8 gives 0.5).
    pub fn beat_length(&self) -> f64 {
        4.0 / f64::from(self.denominator)
    }

    /// Length of a bar in quarter-note beats.
    pub fn bar_length(&self) -> f64 {
        self.beat_length() * f64::from(self.numerator)
    }

    /// Ticks in one signature beat.
    pub fn ticks_per_beat(&self) -> u64 {
        u64::from(PPQ) * 4 / u64::from(self.denominator)
    }

    fn ticks_per_bar(&self) -> u64 {
        self.ticks_per_beat() * u64::from(self.numerator)
    }
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self::COMMON
    }
}

/// Why a time signature or map was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureError {
    /// Numerator outside `1..=64`.
    BadNumerator,
    /// Denominator not a power of two in `1..=64`.
    BadDenominator,
    /// No entries.
    Empty,
    /// The first entry is not at bar 0.
    FirstNotAtBarZero,
    /// Entry `n` is not at a later bar than the previous one.
    NotIncreasing(usize),
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadNumerator => write!(f, "the numerator must be 1 to {MAX_NUMERATOR}"),
            Self::BadDenominator => {
                write!(
                    f,
                    "the denominator must be a power of two up to {MAX_DENOMINATOR}"
                )
            }
            Self::Empty => f.write_str("a time-signature map needs at least one entry"),
            Self::FirstNotAtBarZero => f.write_str("the first time signature must be at bar 0"),
            Self::NotIncreasing(n) => write!(f, "time signature {n} is not at a later bar"),
        }
    }
}

impl std::error::Error for SignatureError {}

/// A musical position: zero-based bar, zero-based signature beat in the bar, and ticks into
/// that beat ([`PPQ`] ticks per quarter note).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BarBeatTick {
    /// Bar, from 0.
    pub bar: u32,
    /// Beat within the bar, from 0.
    pub beat: u32,
    /// Ticks into the beat.
    pub tick: u32,
}

/// A point on the beat grid: where a metronome click or a beat line belongs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    /// Position in quarter-note beats.
    pub beat: f64,
    /// Bar, from 0.
    pub bar: u32,
    /// Signature beat within the bar, from 0 (0 is the downbeat).
    pub index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Entry {
    bar: u32,
    /// Start of the bar in quarter-note beats.
    beat: f64,
    signature: TimeSignature,
}

/// Time signatures over the timeline; a change always starts at a bar line.
///
/// Immutable once built; lookups are binary searches with no allocation, so the audio thread can
/// use them.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeSignatureMap {
    entries: Box<[Entry]>,
}

impl Default for TimeSignatureMap {
    /// 4/4 throughout.
    fn default() -> Self {
        Self::from_signature(TimeSignature::COMMON)
    }
}

impl TimeSignatureMap {
    /// One signature throughout.
    pub fn from_signature(signature: TimeSignature) -> Self {
        Self {
            entries: Box::new([Entry {
                bar: 0,
                beat: 0.0,
                signature,
            }]),
        }
    }

    /// Builds a map from `(bar, signature)` changes ordered by bar; the first must be at bar 0.
    pub fn new(changes: Vec<(u32, TimeSignature)>) -> Result<Self, SignatureError> {
        let first = changes.first().ok_or(SignatureError::Empty)?;
        if first.0 != 0 {
            return Err(SignatureError::FirstNotAtBarZero);
        }
        let mut entries: Vec<Entry> = Vec::with_capacity(changes.len());
        for (index, (bar, signature)) in changes.into_iter().enumerate() {
            let beat = match entries.last() {
                Some(previous) => {
                    if bar <= previous.bar {
                        return Err(SignatureError::NotIncreasing(index));
                    }
                    previous.beat + f64::from(bar - previous.bar) * previous.signature.bar_length()
                }
                None => 0.0,
            };
            entries.push(Entry {
                bar,
                beat,
                signature,
            });
        }
        Ok(Self {
            entries: entries.into_boxed_slice(),
        })
    }

    /// The `(bar, signature)` changes, ordered by bar.
    pub fn changes(&self) -> impl Iterator<Item = (u32, TimeSignature)> + '_ {
        self.entries.iter().map(|e| (e.bar, e.signature))
    }

    fn entry_at_beat(&self, beat: f64) -> &Entry {
        let index = self
            .entries
            .partition_point(|entry| entry.beat <= beat)
            .saturating_sub(1);
        &self.entries[index]
    }

    fn entry_at_bar(&self, bar: u32) -> &Entry {
        let index = self
            .entries
            .partition_point(|entry| entry.bar <= bar)
            .saturating_sub(1);
        &self.entries[index]
    }

    /// The signature in force at `beat`.
    pub fn signature_at_beat(&self, beat: f64) -> TimeSignature {
        self.entry_at_beat(beat).signature
    }

    /// The signature in force in `bar`.
    pub fn signature_at_bar(&self, bar: u32) -> TimeSignature {
        self.entry_at_bar(bar).signature
    }

    /// Position of the start of `bar`, in quarter-note beats.
    pub fn bar_start_beat(&self, bar: u32) -> f64 {
        let entry = self.entry_at_bar(bar);
        entry.beat + f64::from(bar - entry.bar) * entry.signature.bar_length()
    }

    /// The bar, signature beat and tick at `beat` (clamped to the start of the timeline).
    pub fn bar_beat_tick(&self, beat: f64) -> BarBeatTick {
        let beat = if beat.is_finite() { beat.max(0.0) } else { 0.0 };
        let entry = self.entry_at_beat(beat);
        let signature = entry.signature;
        let ticks = ((beat - entry.beat) * f64::from(PPQ) + TICK_SLACK).floor() as u64;
        let bars = ticks / signature.ticks_per_bar();
        let within_bar = ticks % signature.ticks_per_bar();
        BarBeatTick {
            bar: entry.bar + bars as u32,
            beat: (within_bar / signature.ticks_per_beat()) as u32,
            tick: (within_bar % signature.ticks_per_beat()) as u32,
        }
    }

    /// Position in quarter-note beats of a bar/beat/tick. Inverse of
    /// [`bar_beat_tick`](Self::bar_beat_tick) for in-range values.
    pub fn beats_of(&self, position: BarBeatTick) -> f64 {
        let signature = self.signature_at_bar(position.bar);
        let ticks =
            u64::from(position.beat) * signature.ticks_per_beat() + u64::from(position.tick);
        self.bar_start_beat(position.bar) + ticks as f64 / f64::from(PPQ)
    }

    /// The first grid point (signature beat line) at or after `beat`.
    pub fn grid_at_or_after(&self, beat: f64) -> Grid {
        let beat = if beat.is_finite() { beat.max(0.0) } else { 0.0 };
        let index = self
            .entries
            .partition_point(|entry| entry.beat <= beat)
            .saturating_sub(1);
        let entry = &self.entries[index];
        let unit = entry.signature.beat_length();
        let steps = ((beat - entry.beat) / unit - GRID_SLACK).ceil().max(0.0);
        let candidate = entry.beat + steps * unit;
        if let Some(next) = self.entries.get(index + 1)
            && candidate >= next.beat
        {
            return Grid {
                beat: next.beat,
                bar: next.bar,
                index: 0,
            };
        }
        let steps = steps as u32;
        Grid {
            beat: candidate,
            bar: entry.bar + steps / entry.signature.numerator,
            index: steps % entry.signature.numerator,
        }
    }

    /// The grid point after `grid`.
    pub fn next_grid(&self, grid: Grid) -> Grid {
        let unit = self.signature_at_beat(grid.beat).beat_length();
        self.grid_at_or_after(grid.beat + unit * 0.5)
    }
}
