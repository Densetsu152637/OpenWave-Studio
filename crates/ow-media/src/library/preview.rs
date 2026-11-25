//! In-tempo preview: a library loop stretched to the project's tempo.

use std::fmt;

use ow_dsp::stretch::{MAX_RATIO, MIN_RATIO, stretch_to_length};

use super::LibraryEntry;
use crate::Pcm;
use crate::import::{ImportError, ImportLimits, decode_file};

/// Why a preview could not be made.
#[derive(Debug)]
pub enum PreviewError {
    /// The entry has neither a tempo in its name nor a detected one.
    NoTempo,
    /// A tempo is zero, negative or not finite.
    BadTempo,
    /// The tempo change is more than four times either way.
    RatioOutOfRange,
    /// The file could not be decoded.
    Decode(ImportError),
}

impl fmt::Display for PreviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTempo => write!(f, "the file has no known tempo"),
            Self::BadTempo => write!(f, "tempo must be a positive number"),
            Self::RatioOutOfRange => write!(f, "tempo change is beyond 4x in either direction"),
            Self::Decode(e) => write!(f, "decode failed: {e}"),
        }
    }
}

impl std::error::Error for PreviewError {}

/// Frames a clip of `frames` frames recorded at `source_bpm` takes at `target_bpm`.
pub fn warp_target_frames(frames: usize, source_bpm: f64, target_bpm: f64) -> usize {
    (frames as f64 * source_bpm / target_bpm).round() as usize
}

/// Stretches `pcm`, a loop whose tempo is `source_bpm`, to `target_bpm` and shifts it by
/// `pitch_semitones`: the result is `warp_target_frames` long, at the same sample rate and
/// channel count. A loop of whole bars stays a loop of whole bars, and its length is exact.
pub fn preview_in_tempo(
    pcm: &Pcm,
    source_bpm: f64,
    target_bpm: f64,
    pitch_semitones: f32,
) -> Result<Pcm, PreviewError> {
    let valid = |b: f64| b.is_finite() && b > 0.0;
    if !valid(source_bpm) || !valid(target_bpm) {
        return Err(PreviewError::BadTempo);
    }
    let ratio = source_bpm / target_bpm;
    if !(MIN_RATIO..=MAX_RATIO).contains(&ratio) {
        return Err(PreviewError::RatioOutOfRange);
    }
    let views: Vec<&[f32]> = pcm.channels.iter().map(Vec::as_slice).collect();
    let target = warp_target_frames(pcm.frames(), source_bpm, target_bpm);
    let channels = stretch_to_length(
        &views,
        pcm.channels.len(),
        pcm.sample_rate,
        target,
        pitch_semitones,
    );
    Ok(Pcm {
        sample_rate: pcm.sample_rate,
        channels,
    })
}

/// Decodes the file of `entry` and previews it at `target_bpm`, using the entry's own tempo
/// ([`LibraryEntry::bpm`]).
pub fn preview_entry(
    entry: &LibraryEntry,
    target_bpm: f64,
    pitch_semitones: f32,
) -> Result<Pcm, PreviewError> {
    let source = entry.bpm().ok_or(PreviewError::NoTempo)?;
    let pcm = decode_file(&entry.path, &ImportLimits::default()).map_err(PreviewError::Decode)?;
    preview_in_tempo(&pcm, source, target_bpm, pitch_semitones)
}
