//! Media for OpenWave Studio (architecture section 11): import, peak pyramids, disk streaming
//! and the recording writer. This crate depends on no other OpenWave crate except `ow-dsp` (for
//! the library's tempo and key analysis and in-tempo previews); `ow-engine` depends on it for its
//! clip source node.
//!
//! * [`import`]: decode WAV, AIFF, FLAC, MP3 and Ogg Vorbis with symphonia, resample to the
//!   project rate with rubato and copy into the bundle as a content-hash-named 32-bit float WAV
//!   (`audio/<hash>.wav`); bounded by [`import::ImportLimits`], panic-free on hostile files.
//! * [`peaks`]: the min/max pyramid (64, 512 and 4096 frames per entry) stored in
//!   `cache/<hash>.owpk`, and range queries that feed `ow_ipc::peaks`.
//! * [`source`] and [`stream`]: [`AudioSource`], the realtime-safe read interface, implemented
//!   by [`MemorySource`] (short clips, default under 30 s) and [`StreamSource`] (a prefetch
//!   thread fills lock-free blocks ahead of the playhead; an underrun yields silence and bumps
//!   a counter).
//! * [`record`]: [`RecordWriter`], a hound-based writer fed from a lock-free ring.
//! * [`library`]: the loop and sample browser's index (recursive folder scan, tempo and key
//!   detection, tags from names, a versioned JSON cache, search) and in-tempo previews.
//!
//! # Realtime rules
//!
//! Everything reachable from [`AudioSource::read`] and [`RecordProducer::push`] allocates
//! nothing, takes no lock and performs no I/O. Decoding, resampling, file reads and writes and
//! peak computation run on the control thread or on a thread this crate owns.

pub mod analysis;
pub mod import;
pub mod library;
pub mod peaks;
pub mod record;
pub mod source;
pub mod stream;
mod wav;

pub use record::{RecordProducer, RecordWriter};
pub use source::{AudioSource, MemorySource, SourceConfig, open_source};
pub use stream::{StreamHandle, StreamOptions, StreamSource};

/// Decoded audio: planar `f32`, all channels equally long.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    /// Sample rate in hertz.
    pub sample_rate: u32,
    /// One vector per channel.
    pub channels: Vec<Vec<f32>>,
}

impl Pcm {
    /// Frames per channel.
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }
}
