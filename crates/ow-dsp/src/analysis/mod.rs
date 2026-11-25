//! Offline audio analysis for smart tempo and key detection (stage 22).
//!
//! Pure Rust and allocation-happy: it runs on control threads on decoded audio, never on the
//! audio thread. [`detect_tempo`] finds the tempo, first beat and first downbeat of a loop;
//! [`detect_key`] finds its key from a chromagram. Both take one mono channel, see
//! [`mono_mixdown`].

mod fft;
pub mod key;
pub mod tempo;

pub use key::{KeyEstimate, Mode, chroma, detect_key, key_from_chroma};
pub use tempo::{OnsetEnvelope, TempoEstimate, detect_tempo, onset_envelope};

/// Averages `channels` (planar, any number, possibly of different lengths) into one mono
/// buffer as long as the longest channel.
pub fn mono_mixdown(channels: &[&[f32]]) -> Vec<f32> {
    let frames = channels.iter().map(|c| c.len()).max().unwrap_or(0);
    if channels.is_empty() {
        return Vec::new();
    }
    let gain = 1.0 / channels.len() as f32;
    (0..frames)
        .map(|i| {
            channels
                .iter()
                .map(|c| c.get(i).copied().unwrap_or(0.0))
                .sum::<f32>()
                * gain
        })
        .collect()
}
