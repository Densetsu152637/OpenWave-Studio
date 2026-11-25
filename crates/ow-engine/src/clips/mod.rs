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
pub mod warp;

use std::collections::{HashMap, VecDeque};
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

/// The most bytes the warped renders in memory may hold together (the oldest go first).
pub const MAX_WARPED_BYTES: usize = 512 << 20;
/// The most bytes of warp cache files kept in a bundle's `cache/` folder; the least recently
/// used (by modification time, which a cache hit refreshes) are deleted first.
pub const MAX_WARP_FILE_BYTES: u64 = 2 << 30;

/// Warped renders in memory, oldest first.
#[derive(Debug, Default)]
struct WarpStore {
    entries: HashMap<u64, Arc<Pcm>>,
    order: VecDeque<u64>,
    bytes: usize,
}

impl WarpStore {
    fn insert(&mut self, key: u64, pcm: Arc<Pcm>) {
        if self.entries.contains_key(&key) {
            return;
        }
        self.bytes += pcm_bytes(&pcm);
        self.entries.insert(key, pcm);
        self.order.push_back(key);
        while self.bytes > MAX_WARPED_BYTES && self.order.len() > 1 {
            if let Some(old) = self.order.pop_front()
                && let Some(gone) = self.entries.remove(&old)
            {
                self.bytes = self.bytes.saturating_sub(pcm_bytes(&gone));
            }
        }
    }
}

fn pcm_bytes(pcm: &Pcm) -> usize {
    pcm.channels.len() * pcm.frames() * 4
}

/// Loaded media, shared between compiles. Cloning shares the same cache.
#[derive(Debug, Clone, Default)]
pub struct SourceCache {
    entries: Arc<Mutex<HashMap<Key, LoadedMedia>>>,
    warped: Arc<Mutex<WarpStore>>,
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
        *lock(&self.warped) = WarpStore::default();
    }

    /// The warped render with cache key `key` (see [`warp::key`]): from memory, else from the
    /// file `cache/warp-<key>.wav` under `media_root` when it holds `frames` frames of
    /// `channels` channels at `rate` (`shape` is `(rate, channels, frames)`), else made by
    /// `build` (which runs without any lock held) and written there for the next session.
    /// Writing is best effort: a failure only costs the next session its cache.
    pub fn warped(
        &self,
        media_root: &Path,
        key: u64,
        shape: (u32, usize, u64),
        build: impl FnOnce() -> Pcm,
    ) -> Arc<Pcm> {
        if let Some(found) = lock(&self.warped).entries.get(&key) {
            return Arc::clone(found);
        }
        let (rate, channels, frames) = shape;
        let file = media_root
            .join("cache")
            .join(format!("warp-{key:016x}.wav"));
        let from_disk = load_wav(&file).ok().filter(|pcm| {
            pcm.sample_rate == rate
                && pcm.channels.len() == channels
                && pcm.frames() as u64 == frames
        });
        if from_disk.is_some() {
            // A hit makes the file the most recently used one.
            touch(&file);
        }
        let pcm = Arc::new(match from_disk {
            Some(pcm) => pcm,
            None => {
                let pcm = build();
                write_warp_file(&file, &pcm, MAX_WARP_FILE_BYTES);
                pcm
            }
        });
        lock(&self.warped).insert(key, Arc::clone(&pcm));
        pcm
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

/// Sets the modification time of `path` to now (best effort).
fn touch(path: &Path) {
    if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

/// Writes a warp cache file through a temporary file and a rename, then deletes the least
/// recently used warp files until all together hold at most `max_bytes` (the file just
/// written stays). Failures are ignored: the file is only a cache.
pub(crate) fn write_warp_file(path: &Path, pcm: &Pcm, max_bytes: u64) {
    let Some(dir) = path.parent() else { return };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let temp = path.with_extension("tmp");
    if ow_media::import::write_wav(&temp, pcm).is_err() || fs::rename(&temp, path).is_err() {
        let _ = fs::remove_file(&temp);
        return;
    }
    prune_warp_files(dir, path, max_bytes);
}

/// Deletes the oldest `warp-*.wav` files in `dir` (never `keep`) until the rest hold at most
/// `max_bytes`.
pub(crate) fn prune_warp_files(dir: &Path, keep: &Path, max_bytes: u64) {
    let Ok(read) = fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = read
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("warp-") && name.ends_with(".wav")
        })
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, size, path) in files {
        if total <= max_bytes {
            break;
        }
        if path == keep {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny(frames: usize) -> Pcm {
        Pcm {
            sample_rate: 48_000,
            channels: vec![vec![0.25; frames]],
        }
    }

    /// Pruning removes the least recently used files first, keeps the new one and honours the
    /// byte limit; a cache hit refreshes a file's age.
    #[test]
    fn warp_files_are_pruned_by_bytes_least_recently_used_first() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let path = |n: u8| cache.join(format!("warp-{n:016x}.wav"));
        let sources = SourceCache::new();
        for n in 1..=3 {
            let key = u64::from(n);
            sources.warped(dir.path(), key, (48_000, 1, 1_000), || tiny(1_000));
            // Distinct modification times, oldest first.
            let file = fs::OpenOptions::new().write(true).open(path(n)).unwrap();
            file.set_modified(
                std::time::SystemTime::now() - std::time::Duration::from_secs(100 - u64::from(n)),
            )
            .unwrap();
        }
        let size = fs::metadata(path(1)).unwrap().len();
        // A fresh context reads file 1 from disk, which makes it the newest.
        SourceCache::new().warped(dir.path(), 1, (48_000, 1, 1_000), || panic!("cached"));
        write_warp_file(&path(4), &tiny(1_000), size * 2 + size / 2);
        assert!(path(4).is_file(), "the new file stays");
        assert!(path(1).is_file(), "the file that was just used stays");
        assert!(
            !path(2).exists() && !path(3).exists(),
            "the oldest went first"
        );
    }
}
