//! Timeline math: positions in beats, the tempo map and the time-signature map.
//!
//! Conventions, shared with the engine:
//!
//! - A *beat* is a quarter note and positions are `f64` beats from the start of the timeline;
//!   beat 0 is the start of bar 0. Audio positions are 64-bit sample counters.
//! - MIDI resolution is [`PPQ`] (960) ticks per quarter note.
//! - Beat to sample conversion returns an `f64` sample position; the engine's rule for placing
//!   an event on a frame is to round it ([`beats_to_sample_index`]), so a beat always lands on
//!   the same frame regardless of block size.

mod signature;
mod tempo;

#[cfg(test)]
mod tests;

pub use signature::{BarBeatTick, Grid, SignatureError, TimeSignature, TimeSignatureMap};
pub use tempo::{BPM_MAX, BPM_MIN, TempoError, TempoMap, TempoPoint};

/// Ticks per quarter note (MIDI resolution).
pub const PPQ: u32 = 960;

/// Converts beats to the nearest whole tick.
pub fn beats_to_ticks(beats: f64) -> i64 {
    (beats * f64::from(PPQ)).round() as i64
}

/// Converts ticks to beats.
pub fn ticks_to_beats(ticks: i64) -> f64 {
    ticks as f64 / f64::from(PPQ)
}

/// The frame an event at fractional sample position `sample` falls on: the nearest whole
/// sample, never negative.
pub fn sample_index(sample: f64) -> u64 {
    if sample.is_finite() && sample > 0.0 {
        sample.round() as u64
    } else {
        0
    }
}

/// The whole-sample position of `beats` under `tempo` at `sample_rate`; see [`sample_index`].
pub fn beats_to_sample_index(tempo: &TempoMap, beats: f64, sample_rate: u32) -> u64 {
    sample_index(tempo.beats_to_samples(beats, sample_rate))
}
