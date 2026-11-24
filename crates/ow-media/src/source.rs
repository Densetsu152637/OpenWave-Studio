//! The realtime read interface and the in-memory implementation.

use std::path::Path;
use std::sync::Arc;

use crate::Pcm;
use crate::import::ImportError;
use crate::stream::{StreamHandle, StreamOptions, StreamSource};
use crate::wav::wav_info;

/// Audio the engine reads on the audio thread. Implementations never allocate, lock or do I/O
/// in [`read`](Self::read).
pub trait AudioSource: Send {
    /// Channel count (fixed).
    fn channels(&self) -> usize;

    /// Length in frames (fixed).
    fn frames(&self) -> u64;

    /// Writes source frames `position..position + frames` into `out[channel][..frames]`.
    /// Frames at or past the end, and frames a streamed source has not prefetched yet, are
    /// written as silence. `out` holds [`channels`](Self::channels) vectors of at least `frames`
    /// samples; they are never resized.
    fn read(&mut self, position: u64, out: &mut [Vec<f32>], frames: usize);
}

/// A clip held fully in memory; cheap to clone (shared buffer).
#[derive(Debug, Clone)]
pub struct MemorySource {
    pcm: Arc<Pcm>,
}

impl MemorySource {
    /// A source over `pcm`.
    pub fn new(pcm: Arc<Pcm>) -> Self {
        Self { pcm }
    }
}

impl AudioSource for MemorySource {
    fn channels(&self) -> usize {
        self.pcm.channels.len()
    }

    fn frames(&self) -> u64 {
        self.pcm.frames() as u64
    }

    fn read(&mut self, position: u64, out: &mut [Vec<f32>], frames: usize) {
        let total = self.pcm.frames() as u64;
        let start = position.min(total) as usize;
        let have = (total - start as u64).min(frames as u64) as usize;
        for (dst, src) in out.iter_mut().zip(&self.pcm.channels) {
            dst[..have].copy_from_slice(&src[start..start + have]);
            dst[have..frames].fill(0.0);
        }
    }
}

/// How [`open_source`] chooses between memory and streaming.
#[derive(Debug, Clone, Copy)]
pub struct SourceConfig {
    /// Files up to this many seconds play from memory; longer ones stream. Default 30.
    pub stream_threshold_secs: f64,
    /// Streaming buffer sizes.
    pub stream: StreamOptions,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            stream_threshold_secs: 30.0,
            stream: StreamOptions::default(),
        }
    }
}

/// Reads a bundle media file (the 32-bit float WAV [`import`](crate::import) writes) fully.
/// The header is not trusted: channels are capped and the length is clamped to the file.
pub fn load_wav(path: &Path) -> Result<Pcm, ImportError> {
    let wav_err = |e: hound::Error| ImportError::Decode(e.to_string());
    let info = wav_info(path, true)?;
    let channels = usize::from(info.spec.channels);
    let frames = info.frames as usize;
    let mut reader = hound::WavReader::open(path).map_err(wav_err)?;
    let mut planar = vec![Vec::with_capacity(frames); channels];
    for (i, sample) in reader.samples::<f32>().take(frames * channels).enumerate() {
        planar[i % channels].push(sample.map_err(wav_err)?);
    }
    Ok(Pcm {
        sample_rate: info.spec.sample_rate,
        channels: planar,
    })
}

/// Opens a bundle media file for playback: in memory when it is at most the configured
/// threshold long, otherwise streamed from disk (the handle controls and reports on the stream).
pub fn open_source(
    path: &Path,
    config: &SourceConfig,
) -> Result<(Box<dyn AudioSource>, Option<StreamHandle>), ImportError> {
    let info = wav_info(path, true)?;
    let seconds = info.frames as f64 / f64::from(info.spec.sample_rate);
    if seconds <= config.stream_threshold_secs {
        let pcm = load_wav(path)?;
        Ok((Box::new(MemorySource::new(Arc::new(pcm))), None))
    } else {
        let source = StreamSource::open(path, config.stream)?;
        let handle = source.handle();
        Ok((Box::new(source), Some(handle)))
    }
}
