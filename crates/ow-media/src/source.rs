//! The realtime read interface and the in-memory implementation.

use std::path::Path;
use std::sync::Arc;

use crate::Pcm;
use crate::import::ImportError;
use crate::stream::{StreamHandle, StreamOptions, StreamSource};

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
pub fn load_wav(path: &Path) -> Result<Pcm, ImportError> {
    let wav_err = |e: hound::Error| ImportError::Decode(e.to_string());
    let mut reader = hound::WavReader::open(path).map_err(wav_err)?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Float || spec.bits_per_sample != 32 {
        return Err(ImportError::Decode("media is not 32-bit float WAV".into()));
    }
    let channels = usize::from(spec.channels);
    let mut planar = vec![Vec::with_capacity(reader.duration() as usize); channels];
    for (i, sample) in reader.samples::<f32>().enumerate() {
        planar[i % channels].push(sample.map_err(wav_err)?);
    }
    Ok(Pcm {
        sample_rate: spec.sample_rate,
        channels: planar,
    })
}

/// Opens a bundle media file for playback: in memory when it is at most the configured
/// threshold long, otherwise streamed from disk (the handle controls and reports on the stream).
pub fn open_source(
    path: &Path,
    config: &SourceConfig,
) -> Result<(Box<dyn AudioSource>, Option<StreamHandle>), ImportError> {
    let reader = hound::WavReader::open(path).map_err(|e| ImportError::Decode(e.to_string()))?;
    let spec = reader.spec();
    let seconds = f64::from(reader.duration()) / f64::from(spec.sample_rate.max(1));
    drop(reader);
    if seconds <= config.stream_threshold_secs {
        let pcm = load_wav(path)?;
        Ok((Box::new(MemorySource::new(Arc::new(pcm))), None))
    } else {
        let source = StreamSource::open(path, config.stream)?;
        let handle = source.handle();
        Ok((Box::new(source), Some(handle)))
    }
}
