//! Loading the audio that clips play, for the compiler.
//!
//! MVP clips always play from memory: [`SourceCache::load`] reads a bundle media file with
//! [`ow_media::source::load_wav`] and, when it is not at the engine's sample rate, resamples it
//! once with [`ow_media::import::resample`]. Both happen on the control thread while a project
//! compiles; the audio thread only ever reads the finished buffers. The result is cached as an
//! [`Arc`] keyed by (absolute path, engine rate), so recompiling after an edit reuses what is
//! already in memory and clips of the same media share one buffer. Media files are named by
//! content hash, so a path never changes meaning while the cache holds it.
//!
//! Entries stay until [`SourceCache::retain_under`] drops those outside the current media root
//! (the app calls it when the root changes); media that left the project stays cached until then.

pub mod midi;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use ow_media::Pcm;
use ow_media::import::{ImportError, ImportLimits, resample};
use ow_media::source::load_wav;

/// A media file in memory at the engine's sample rate.
#[derive(Debug, Clone)]
pub struct LoadedMedia {
    /// The audio, at the engine rate.
    pub pcm: Arc<Pcm>,
    /// The sample rate the file itself is stored at. Clip source offsets are counted in frames
    /// of the file, so the compiler scales them by `engine rate / source rate`.
    pub source_rate: u32,
}

type Key = (PathBuf, u32);

/// Loaded media, shared between compiles. Cloning shares the same cache.
#[derive(Debug, Clone, Default)]
pub struct SourceCache {
    entries: Arc<Mutex<HashMap<Key, LoadedMedia>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl SourceCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The media file at `path` as audio at `rate`, from the cache when it is there. A file that
    /// is missing, unreadable, too large or not a bundle WAV is an error and is not cached, so
    /// it is tried again at the next compile (the file may have appeared).
    pub fn load(&self, path: &Path, rate: u32) -> Result<LoadedMedia, ImportError> {
        let key = (
            std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()),
            rate,
        );
        if let Some(found) = lock(&self.entries).get(&key) {
            return Ok(found.clone());
        }
        let limits = ImportLimits::default();
        // `load_wav` allocates for the whole file: refuse what import would have refused.
        if fs::metadata(path)?.len() > limits.max_samples * 4 + 65_536 {
            return Err(ImportError::TooLarge("media file size"));
        }
        let pcm = load_wav(path)?;
        let source_rate = pcm.sample_rate;
        let pcm = resample(pcm, rate, limits.max_samples)?;
        let loaded = LoadedMedia {
            pcm: Arc::new(pcm),
            source_rate,
        };
        lock(&self.entries).insert(key, loaded.clone());
        Ok(loaded)
    }

    /// Drops every entry whose file is not inside `root`.
    pub fn retain_under(&self, root: &Path) {
        let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
        lock(&self.entries).retain(|(path, _), _| path.starts_with(&root));
    }

    /// Drops everything.
    pub fn clear(&self) {
        lock(&self.entries).clear();
    }

    /// Number of cached (file, rate) pairs.
    pub fn len(&self) -> usize {
        lock(&self.entries).len()
    }

    /// True when nothing is cached.
    pub fn is_empty(&self) -> bool {
        lock(&self.entries).is_empty()
    }
}
