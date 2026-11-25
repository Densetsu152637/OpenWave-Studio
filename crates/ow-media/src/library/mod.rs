//! The loop and sample library (stage 22): a scan of local folders into a searchable index.
//!
//! * [`scan`] walks the root folders recursively (a root itself may be a link; symbolic links
//!   found inside, to files or to folders, are skipped, so links can neither loop nor leave
//!   the library),
//!   decodes every WAV, AIFF, FLAC, Ogg Vorbis and MP3 file with [`import::decode_file`], and
//!   records its duration, rate, channel count, a tempo and key detected by `ow_dsp::analysis`
//!   on a mono mixdown, and tags from folder names and file-name tokens (see [`tags`]).
//!   A tempo or key written in the file name (`120bpm`, `Am`) is kept apart from the detected
//!   one and wins in [`LibraryEntry::bpm`] and [`LibraryEntry::key`].
//! * The result is a [`LibraryIndex`], cached as versioned JSON keyed by path, size and
//!   modification time (plus [`ANALYSIS_VERSION`] and the size limit in force), like the plugin
//!   scan cache: unchanged files are not decoded again, a changed file is analysed again, a
//!   vanished file drops out, and a cache with another [`INDEX_VERSION`] or unreadable JSON is ignored. Files that fail to decode are cached too
//!   (with the error), so they are not retried until they change.
//! * [`LibraryIndex::search`] filters by text, tags, tempo range and key.
//! * [`preview_in_tempo`] returns a loop stretched to a target tempo with
//!   `ow_dsp::stretch::stretch_to_length`.
//!
//! Scanning allocates and reads whole files: run it on a worker thread, never on the audio
//! thread. Files larger than [`ScanOptions::max_file_bytes`] (128 MiB by default) are listed
//! without analysis, and files that cannot be statted are listed with an error. Memory is
//! bounded by at most [`MAX_WORKERS`] files being decoded at once, each limited to 2^26
//! samples (256 MiB), so a scan peaks near 1.3 GiB however large the library.

pub mod tags;

mod preview;

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use ow_dsp::analysis::{detect_key, detect_tempo, mono_mixdown};
use serde::{Deserialize, Serialize};

use crate::Pcm;
use crate::import::{ImportLimits, decode_file};

pub use preview::{PreviewError, preview_entry, preview_in_tempo, warp_target_frames};
pub use tags::{LibraryKey, NameTokens, parse_name, path_tags};

/// Version of the cached index format; a cache with another version is ignored.
pub const INDEX_VERSION: u32 = 1;
/// Version of the analysis (decoding limits, tempo and key algorithms); bump it when results
/// can change, so cached entries are analysed again.
pub const ANALYSIS_VERSION: u32 = 1;
/// Most workers decoding at once; each holds one decoded file (at most 256 MiB of samples) and
/// its mono mixdown, so peak memory is about this many times 320 MiB.
pub const MAX_WORKERS: usize = 4;
/// File extensions the scan picks up (lower case).
pub const AUDIO_EXTENSIONS: [&str; 6] = ["wav", "flac", "ogg", "mp3", "aif", "aiff"];
/// Detected tempi below this confidence are dropped: a sustained pad has no beat to report.
pub const MIN_TEMPO_CONFIDENCE: f64 = 0.3;
/// Deepest folder nesting the scan descends into.
const MAX_DEPTH: usize = 32;

/// One file in the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryEntry {
    /// Absolute or root-relative path as scanned (valid UTF-8).
    pub path: PathBuf,
    /// File size in bytes (cache key).
    pub size: u64,
    /// Modification time in milliseconds since the Unix epoch (cache key).
    pub modified_ms: u64,
    /// Length in seconds; 0 when the file was not decoded.
    pub duration_seconds: f64,
    /// Sample rate of the file in hertz; 0 when not decoded.
    pub sample_rate: u32,
    /// Channel count; 0 when not decoded.
    pub channels: u16,
    /// Tempo detected from the audio, when it is long enough and rhythmic enough to tell (see
    /// [`MIN_TEMPO_CONFIDENCE`]).
    pub bpm_detected: Option<f64>,
    /// How sure the tempo detection was, 0 to 1.
    pub bpm_confidence: Option<f64>,
    /// Tempo written in the file name.
    pub bpm_tag: Option<f64>,
    /// Key detected from the audio.
    pub key_detected: Option<LibraryKey>,
    /// How sure the key detection was, 0 to 1.
    pub key_confidence: Option<f64>,
    /// Key written in the file name.
    pub key_tag: Option<LibraryKey>,
    /// Tags from folder and file names; recomputed on every scan, so renaming folders never
    /// needs a re-analysis.
    pub tags: Vec<String>,
    /// Why the file could not be analysed (undecodable, too large, unreadable), when so.
    pub error: Option<String>,
    /// [`ANALYSIS_VERSION`] this entry was analysed with (cache key).
    pub analysis_version: u32,
    /// The [`ScanOptions::max_file_bytes`] in force when it was analysed (cache key: raising
    /// the limit re-analyses files that were skipped as too large).
    pub max_file_bytes: u64,
}

impl LibraryEntry {
    /// The tempo to use: the file name's when it has one, otherwise the detected one.
    pub fn bpm(&self) -> Option<f64> {
        self.bpm_tag.or(self.bpm_detected)
    }

    /// The key to use: the file name's when it has one, otherwise the detected one.
    pub fn key(&self) -> Option<LibraryKey> {
        self.key_tag.or(self.key_detected)
    }

    /// The file name without its folder.
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// The scanned library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryIndex {
    /// Format version, see [`INDEX_VERSION`].
    pub version: u32,
    /// All files, sorted by path.
    pub entries: Vec<LibraryEntry>,
}

impl Default for LibraryIndex {
    fn default() -> Self {
        Self {
            version: INDEX_VERSION,
            entries: Vec::new(),
        }
    }
}

/// A library search; every given criterion must match.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// Words that must each appear (case-insensitively) in the file name, folder path or tags.
    pub text: String,
    /// Tags that must all be present (case-insensitive).
    pub tags: Vec<String>,
    /// Lowest tempo, inclusive.
    pub bpm_min: Option<f64>,
    /// Highest tempo, inclusive.
    pub bpm_max: Option<f64>,
    /// Also accept files whose tempo is half or double a tempo inside the range.
    pub include_half_double: bool,
    /// Required key.
    pub key: Option<LibraryKey>,
}

impl LibraryIndex {
    /// The entries matching `query`, in path order.
    pub fn search(&self, query: &Query) -> Vec<&LibraryEntry> {
        let words: Vec<String> = query
            .text
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let tags: Vec<String> = query.tags.iter().map(|t| t.to_lowercase()).collect();
        self.entries
            .iter()
            .filter(|entry| {
                let haystack = format!(
                    "{} {}",
                    entry.path.to_string_lossy().to_lowercase(),
                    entry.tags.join(" ")
                );
                words.iter().all(|w| haystack.contains(w.as_str()))
                    && tags.iter().all(|t| entry.tags.iter().any(|e| e == t))
                    && tempo_matches(entry, query)
                    && query.key.is_none_or(|k| entry.key() == Some(k))
            })
            .collect()
    }

    /// The entry for `path`, if indexed.
    pub fn find(&self, path: &Path) -> Option<&LibraryEntry> {
        self.entries.iter().find(|e| e.path == path)
    }
}

fn tempo_matches(entry: &LibraryEntry, query: &Query) -> bool {
    if query.bpm_min.is_none() && query.bpm_max.is_none() {
        return true;
    }
    let Some(bpm) = entry.bpm() else {
        return false;
    };
    let inside = |b: f64| {
        query.bpm_min.is_none_or(|min| b >= min) && query.bpm_max.is_none_or(|max| b <= max)
    };
    inside(bpm) || (query.include_half_double && (inside(bpm * 2.0) || inside(bpm / 2.0)))
}

/// Shared handles to follow and stop a running [`scan`]: the caller keeps a clone, the scan
/// updates it. Counts are in files; they are valid once the scan has listed the folders
/// (`total` is 0 until then).
#[derive(Debug, Clone, Default)]
pub struct ScanControl {
    /// Set to true to stop the scan: files being analysed finish, no new ones start and the
    /// report comes back with [`ScanReport::cancelled`] set.
    pub cancel: Arc<AtomicBool>,
    /// Files handled so far (answered from the cache or analysed).
    pub done: Arc<AtomicUsize>,
    /// Files the scan found.
    pub total: Arc<AtomicUsize>,
}

impl ScanControl {
    /// A fresh control with nothing cancelled and nothing counted.
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the scan to stop.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Whether [`cancel`](Self::cancel) was called.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// `(done, total)` files.
    pub fn progress(&self) -> (usize, usize) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }
}

/// How [`scan`] works.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Where the index cache lives; `None` keeps nothing between scans.
    pub cache_path: Option<PathBuf>,
    /// Files larger than this are listed without being decoded or analysed.
    pub max_file_bytes: u64,
    /// Ignore the cache and analyse everything again.
    pub force: bool,
    /// Worker threads for decoding and analysis; 0 picks the machine's parallelism, and any
    /// value is capped at [`MAX_WORKERS`] to bound memory.
    pub threads: usize,
    /// Handles to follow and cancel the scan; `None` runs it to the end unobserved.
    pub control: Option<ScanControl>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            cache_path: None,
            max_file_bytes: 128 << 20,
            force: false,
            threads: 0,
            control: None,
        }
    }
}

/// What a scan did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanReport {
    /// The resulting index.
    pub index: LibraryIndex,
    /// Files decoded and analysed in this scan.
    pub analysed: usize,
    /// Files answered from the cache.
    pub cached: usize,
    /// Files that could not be decoded or were too large (they are in the index with an error).
    pub failed: usize,
    /// The scan was stopped through [`ScanControl::cancel`]: the index lists the files handled
    /// before that (files not reached are left out, and the cache keeps them for the next scan).
    pub cancelled: bool,
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Collects audio files below `dir` (UTF-8 paths only), depth first, in name order.
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<_> = read.filter_map(Result::ok).collect();
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        // `file_type` does not follow symbolic links, so linked folders are skipped.
        let Ok(kind) = child.file_type() else {
            continue;
        };
        let path = child.path();
        if kind.is_dir() {
            walk(&path, depth + 1, out);
        } else if kind.is_file() && is_audio(&path) && path.to_str().is_some() {
            out.push(path);
        }
    }
}

fn fingerprint(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((meta.len(), u64::try_from(modified.as_millis()).ok()?))
}

/// The index cached at `path` by an earlier [`scan`] (empty when there is none, or it is from
/// another version or damaged); what a search can use before the first scan of a session.
pub fn load_index(path: &Path) -> LibraryIndex {
    load_cache(path)
}

fn load_cache(path: &Path) -> LibraryIndex {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<LibraryIndex>(&bytes).ok())
        .filter(|index| index.version == INDEX_VERSION)
        .unwrap_or_default()
}

/// Writes through a temporary file and a rename; a failure only costs the cache.
fn save_cache(path: &Path, index: &LibraryIndex) {
    let Ok(bytes) = serde_json::to_vec_pretty(index) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let temp = path.with_extension("tmp");
    let written = fs::File::create(&temp)
        .and_then(|mut file| file.write_all(&bytes).and_then(|()| file.sync_all()))
        .and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
}

/// An entry with no analysis results.
fn blank(path: &Path, size: u64, modified_ms: u64, options: &ScanOptions) -> LibraryEntry {
    LibraryEntry {
        path: path.to_path_buf(),
        size,
        modified_ms,
        duration_seconds: 0.0,
        sample_rate: 0,
        channels: 0,
        bpm_detected: None,
        bpm_confidence: None,
        bpm_tag: None,
        key_detected: None,
        key_confidence: None,
        key_tag: None,
        tags: Vec::new(),
        error: None,
        analysis_version: ANALYSIS_VERSION,
        max_file_bytes: options.max_file_bytes,
    }
}

/// The tempo and key found in a piece of audio.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Analysis {
    /// Tempo in beats per minute (rounded to 0.01), when the detection was at least
    /// [`MIN_TEMPO_CONFIDENCE`] sure.
    pub bpm: Option<f64>,
    /// How sure the tempo detection was, 0 to 1.
    pub bpm_confidence: Option<f64>,
    /// The key, when one was found.
    pub key: Option<LibraryKey>,
    /// How sure the key detection was, 0 to 1.
    pub key_confidence: Option<f64>,
}

/// Detects the tempo and key of `pcm` on a mono mixdown (the same analysis a scan and an
/// import run). Takes time proportional to the audio (at most the first two minutes are
/// looked at); control threads only.
pub fn analyse_pcm(pcm: &Pcm) -> Analysis {
    let views: Vec<&[f32]> = pcm.channels.iter().map(Vec::as_slice).collect();
    let mono = mono_mixdown(&views);
    let mut found = Analysis::default();
    if let Some(tempo) = detect_tempo(&mono, pcm.sample_rate)
        && tempo.confidence >= MIN_TEMPO_CONFIDENCE
    {
        found.bpm = Some((tempo.bpm * 100.0).round() / 100.0);
        found.bpm_confidence = Some(tempo.confidence);
    }
    if let Some(key) = detect_key(&mono, pcm.sample_rate) {
        found.key = Some(LibraryKey {
            tonic: key.tonic,
            minor: key.mode == ow_dsp::analysis::Mode::Minor,
        });
        found.key_confidence = Some(key.confidence);
    }
    found
}

/// Decodes and analyses one file into an entry without tags.
fn analyse(path: &Path, size: u64, modified_ms: u64, options: &ScanOptions) -> LibraryEntry {
    let mut entry = blank(path, size, modified_ms, options);
    if size > options.max_file_bytes {
        entry.error = Some("file too large to analyse".into());
        return entry;
    }
    let limits = ImportLimits {
        max_file_bytes: options.max_file_bytes,
        // 2^26 samples: at most 256 MiB of decoded audio per worker.
        max_samples: 1 << 26,
        ..ImportLimits::default()
    };
    match decode_file(path, &limits) {
        Err(error) => entry.error = Some(error.to_string()),
        Ok(pcm) => {
            entry.sample_rate = pcm.sample_rate;
            entry.channels = u16::try_from(pcm.channels.len()).unwrap_or(u16::MAX);
            entry.duration_seconds = pcm.frames() as f64 / f64::from(pcm.sample_rate.max(1));
            let found = analyse_pcm(&pcm);
            entry.bpm_detected = found.bpm;
            entry.bpm_confidence = found.bpm_confidence;
            entry.key_detected = found.key;
            entry.key_confidence = found.key_confidence;
        }
    }
    entry
}

/// Scans `roots` into an index, using and refreshing the cache named in `options`.
pub fn scan(roots: &[PathBuf], options: &ScanOptions) -> ScanReport {
    let cache = match (&options.cache_path, options.force) {
        (Some(path), false) => load_cache(path),
        _ => LibraryIndex::default(),
    };
    let mut files: Vec<(PathBuf, usize)> = Vec::new();
    for (root_index, root) in roots.iter().enumerate() {
        let mut found = Vec::new();
        if root.is_file() && is_audio(root) {
            found.push(root.clone());
        } else {
            walk(root, 0, &mut found);
        }
        files.extend(found.into_iter().map(|p| (p, root_index)));
    }
    files.sort();
    files.dedup_by(|a, b| a.0 == b.0);
    let control = options.control.clone().unwrap_or_default();
    control.total.store(files.len(), Ordering::Relaxed);
    control.done.store(0, Ordering::Relaxed);

    // Decide per file: cache hit, or analyse (in parallel below).
    let mut slots: Vec<Option<LibraryEntry>> = vec![None; files.len()];
    let mut todo: Vec<(usize, u64, u64)> = Vec::new();
    let mut cached = 0;
    let known: HashMap<&Path, &LibraryEntry> = cache
        .entries
        .iter()
        .map(|e| (e.path.as_path(), e))
        .collect();
    for (i, (path, _)) in files.iter().enumerate() {
        let Some((size, modified)) = fingerprint(path) else {
            // Listed, with the reason, so the user sees the file instead of it vanishing.
            let mut entry = blank(path, 0, 0, options);
            entry.error = Some("cannot read file metadata".into());
            slots[i] = Some(entry);
            control.done.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let hit = known.get(path.as_path()).filter(|e| {
            e.size == size
                && e.modified_ms == modified
                && e.analysis_version == ANALYSIS_VERSION
                && e.max_file_bytes == options.max_file_bytes
        });
        match hit {
            Some(entry) => {
                slots[i] = Some((*entry).clone());
                cached += 1;
                control.done.fetch_add(1, Ordering::Relaxed);
            }
            None => todo.push((i, size, modified)),
        }
    }

    let threads = if options.threads == 0 {
        std::thread::available_parallelism().map_or(2, std::num::NonZero::get)
    } else {
        options.threads
    }
    .clamp(1, MAX_WORKERS)
    .min(todo.len().max(1));
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, LibraryEntry)>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    if control.is_cancelled() {
                        return;
                    }
                    let job = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(slot, size, modified)) = todo.get(job) else {
                        return;
                    };
                    let entry = analyse(&files[slot].0, size, modified, options);
                    control.done.fetch_add(1, Ordering::Relaxed);
                    results
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((slot, entry));
                }
            });
        }
    });
    let cancelled = control.is_cancelled();
    let analysed = results
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len();
    for (slot, entry) in results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        slots[slot] = Some(entry);
    }

    // Tags depend on the path and the root, so they are derived on every scan.
    let mut entries = Vec::with_capacity(slots.len());
    for (slot, (path, root_index)) in slots.into_iter().zip(&files) {
        let Some(mut entry) = slot else { continue };
        let root = roots
            .get(*root_index)
            .map_or(Path::new(""), PathBuf::as_path);
        let root = if root.is_file() {
            root.parent().unwrap_or(Path::new(""))
        } else {
            root
        };
        let name = parse_name(
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default(),
        );
        entry.bpm_tag = name.bpm;
        entry.key_tag = name.key;
        entry.tags = path_tags(root, path);
        entries.push(entry);
    }
    let failed = entries.iter().filter(|e| e.error.is_some()).count();
    let index = LibraryIndex {
        version: INDEX_VERSION,
        entries,
    };
    // A stopped scan lists only the files it reached; writing that would make the cache forget
    // the others, so the cache keeps what the last complete scan found.
    if let Some(path) = options.cache_path.as_ref().filter(|_| !cancelled) {
        save_cache(path, &index);
    }
    ScanReport {
        index,
        analysed,
        cached,
        failed,
        cancelled,
    }
}

#[cfg(test)]
mod tests;
