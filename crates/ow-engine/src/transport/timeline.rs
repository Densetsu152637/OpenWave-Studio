//! The tempo map and time-signature map as the audio thread sees them.

use ow_model::time::{TempoMap, TimeSignatureMap, beats_to_sample_index};

/// Immutable timeline description: tempo map plus time-signature map.
///
/// The control thread builds one and swaps it in with
/// [`ControlLink::swap_timeline`](crate::ControlLink::swap_timeline); the audio thread keeps it
/// until the next swap and hands the old one back through a retire ring, so it is never freed on
/// the audio thread. Every query here is a binary search and a closed-form evaluation: no locks,
/// no allocation.
///
/// Sample positions are measured from beat 0. Placing a beat on a frame always uses
/// [`beat_to_sample_index`](Self::beat_to_sample_index) (round to the nearest sample), the same
/// rule the transport uses for loop and punch bounds and the metronome uses for clicks, so all
/// of them agree on which frame a beat falls on.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Timeline {
    tempo: TempoMap,
    signatures: TimeSignatureMap,
}

impl Timeline {
    /// A timeline from a tempo map and a time-signature map.
    pub fn new(tempo: TempoMap, signatures: TimeSignatureMap) -> Self {
        Self { tempo, signatures }
    }

    /// The tempo map.
    pub fn tempo(&self) -> &TempoMap {
        &self.tempo
    }

    /// The time-signature map.
    pub fn signatures(&self) -> &TimeSignatureMap {
        &self.signatures
    }

    /// Fractional sample position of `beat` at `sample_rate`.
    pub fn beat_to_sample(&self, beat: f64, sample_rate: u32) -> f64 {
        self.tempo.beats_to_samples(beat, sample_rate)
    }

    /// The frame `beat` falls on: its sample position rounded to the nearest whole sample.
    pub fn beat_to_sample_index(&self, beat: f64, sample_rate: u32) -> u64 {
        beats_to_sample_index(&self.tempo, beat, sample_rate)
    }

    /// The beat at whole sample position `sample`.
    pub fn sample_to_beat(&self, sample: u64, sample_rate: u32) -> f64 {
        self.tempo.samples_to_beats(sample as f64, sample_rate)
    }
}
