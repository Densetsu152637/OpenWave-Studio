//! The loop and sample library (stage 22b): scanning the library folders, searching the index
//! and auditioning a file in the project's tempo.
//!
//! The scan itself is `ow_media::library::scan` (cache, progress, cancellation); this module keeps
//! the last index in [`LibraryState`] so searches never wait for a scan, runs at most one scan at
//! a time and plays previews through the engine's preview player. Only files that are in the
//! index can be previewed, so an agent cannot make the app decode arbitrary paths this way.
//!
//! Everything here runs on a blocking worker, never on the audio thread; the preview is decoded
//! and stretched before it is handed to the engine, which only mixes finished audio.

use std::path::{Path, PathBuf};

use ow_engine::Engine;
use ow_ipc::{
    LibraryEntryInfo, LibraryKey, LibraryPreviewReply, LibraryQuery, LibraryScanStatus,
    LibrarySearchReply,
};
use ow_media::import::{ImportLimits, decode_file};
use ow_media::library::{
    LibraryEntry, LibraryIndex, Query, ScanControl, ScanOptions, load_index, preview_in_tempo, scan,
};

use super::{IpcState, lock};

/// Longest file the audition takes, in seconds (a loop browser previews loops, not songs).
const MAX_PREVIEW_SECONDS: f64 = 120.0;
/// Entries a search answers when the query names no limit, and the most it ever answers.
const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 2_000;

/// The library as the app holds it between commands.
#[derive(Default)]
pub struct LibraryState {
    index: LibraryIndex,
    /// The index has been loaded from the cache or by a scan.
    loaded: bool,
    /// The handles of the scan that is running.
    running: Option<ScanControl>,
    /// Result of the last finished scan.
    last: LibraryScanStatus,
}

fn info(entry: &LibraryEntry) -> LibraryEntryInfo {
    LibraryEntryInfo {
        path: entry.path.to_string_lossy().into_owned(),
        name: entry.file_name(),
        folder: entry
            .path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        duration_seconds: entry.duration_seconds,
        sample_rate: entry.sample_rate,
        channels: entry.channels,
        bpm: entry.bpm(),
        bpm_detected: entry.bpm_tag.is_none() && entry.bpm_detected.is_some(),
        key: entry.key().map(|k| LibraryKey {
            tonic: k.tonic,
            minor: k.minor,
        }),
        tags: entry.tags.clone(),
        error: entry.error.clone(),
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl IpcState {
    /// Scans `roots` (the library folders of the settings) and keeps the result for searches.
    /// Unchanged files come from the cache at `cache` unless `force`. Blocks until the scan is
    /// done or cancelled; a second scan while one runs is refused. Progress is
    /// [`library_status`](Self::library_status).
    pub fn library_scan(
        &self,
        roots: &[PathBuf],
        cache: &Path,
        force: bool,
    ) -> Result<LibraryScanStatus, String> {
        let control = ScanControl::new();
        {
            let mut library = lock(&self.library);
            if library.running.is_some() {
                return Err("a library scan is already running".into());
            }
            if roots.is_empty() {
                return Err(
                    "no library folders: add some in Settings, Folders (paths.libraryDirs)".into(),
                );
            }
            library.running = Some(control.clone());
        }
        let report = scan(
            roots,
            &ScanOptions {
                cache_path: Some(cache.to_path_buf()),
                force,
                control: Some(control),
                ..ScanOptions::default()
            },
        );
        let mut library = lock(&self.library);
        library.running = None;
        library.last = LibraryScanStatus {
            scanning: false,
            done: 0,
            total: 0,
            entries: count(report.index.entries.len()),
            analysed: count(report.analysed),
            cached: count(report.cached),
            failed: count(report.failed),
            cancelled: report.cancelled,
            roots: roots
                .iter()
                .map(|r| r.to_string_lossy().into_owned())
                .collect(),
        };
        // A cancelled scan lists only what it reached; keep the earlier index when it knew more.
        if !report.cancelled || report.index.entries.len() >= library.index.entries.len() {
            library.index = report.index;
        }
        library.loaded = true;
        Ok(library.last.clone())
    }

    /// The running scan's progress, or the last scan's result.
    pub fn library_status(&self) -> LibraryScanStatus {
        let library = lock(&self.library);
        let mut status = library.last.clone();
        status.entries = count(library.index.entries.len());
        if let Some(control) = &library.running {
            let (done, total) = control.progress();
            status.scanning = true;
            status.done = count(done);
            status.total = count(total);
        }
        status
    }

    /// Stops the running scan (it finishes the files in flight); true when there was one.
    pub fn library_cancel(&self) -> bool {
        match &lock(&self.library).running {
            Some(control) => {
                control.cancel();
                true
            }
            None => false,
        }
    }

    /// Searches the library index; before the first scan of the session it reads the cache at
    /// `cache`, so the browser has something to show at once.
    pub fn library_search(&self, query: &LibraryQuery, cache: &Path) -> LibrarySearchReply {
        let mut library = lock(&self.library);
        if !library.loaded {
            library.index = load_index(cache);
            library.loaded = true;
        }
        let found = library.index.search(&Query {
            text: query.text.clone(),
            tags: query.tags.clone(),
            bpm_min: query.bpm_min,
            bpm_max: query.bpm_max,
            include_half_double: query.include_half_double,
            key: query.key.map(|k| ow_media::library::LibraryKey {
                tonic: k.tonic,
                minor: k.minor,
            }),
        });
        let limit = match query.limit as usize {
            0 => DEFAULT_LIMIT,
            n => n.min(MAX_LIMIT),
        };
        LibrarySearchReply {
            total: count(library.index.entries.len()),
            matched: count(found.len()),
            entries: found.into_iter().take(limit).map(info).collect(),
            scanning: library.running.is_some(),
        }
    }

    /// Plays the library file at `path` through the engine, stretched to the project's tempo at
    /// the start (a file without a tempo plays as it is) and shifted by `semitones`. Replaces a
    /// preview that is playing. The file must be in the library index.
    pub fn library_preview(
        &self,
        engine: &Engine,
        path: &Path,
        semitones: f64,
    ) -> Result<LibraryPreviewReply, String> {
        let entry = lock(&self.library)
            .index
            .find(path)
            .cloned()
            .ok_or("that file is not in the library; scan the library folders first")?;
        if let Some(error) = &entry.error {
            return Err(format!("{} cannot be played: {error}", entry.file_name()));
        }
        if entry.duration_seconds > MAX_PREVIEW_SECONDS {
            return Err(format!(
                "{} is longer than {MAX_PREVIEW_SECONDS} seconds; the preview is for loops and samples",
                entry.file_name()
            ));
        }
        let target = lock(&self.control)
            .session()
            .project()
            .tempo_map
            .bpm_at(0.0);
        let pitch = semitones.clamp(-24.0, 24.0) as f32;
        let source = entry.bpm();
        let pcm = decode_file(&entry.path, &ImportLimits::default())
            .map_err(|error| format!("could not decode {}: {error}", entry.file_name()))?;
        let pcm = match source {
            Some(source) if (source - target).abs() > 1e-6 || pitch != 0.0 => {
                preview_in_tempo(&pcm, source, target, pitch).map_err(|error| error.to_string())?
            }
            _ => pcm,
        };
        let seconds = pcm.frames() as f64 / f64::from(pcm.sample_rate.max(1));
        engine.start_preview(pcm)?;
        Ok(LibraryPreviewReply {
            playing: true,
            source_bpm: source,
            target_bpm: source.map(|_| target),
            seconds,
        })
    }

    /// Stops the preview.
    pub fn library_preview_stop(&self, engine: &Engine) {
        engine.stop_preview();
    }
}

/// Where the library index is cached.
pub fn index_cache_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("library-index.json")
}

/// The library folders of the settings as paths.
pub fn roots_of(dirs: &[String]) -> Vec<PathBuf> {
    dirs.iter().map(PathBuf::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_loop(path: &Path, bpm: f64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        let beat = (60.0 / bpm * 44_100.0) as usize;
        for i in 0..beat * 16 {
            let into = i % beat;
            let value = if into < 2_000 {
                (f64::from(i as u32) * 0.2).sin() * 0.8 * (1.0 - into as f64 / 2_000.0)
            } else {
                0.0
            };
            writer.write_sample((value * 20_000.0) as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn scan_search_and_status_over_a_temporary_library() {
        let dir = tempfile::tempdir().unwrap();
        write_loop(&dir.path().join("Drums/120bpm_kick.wav"), 120.0);
        write_loop(&dir.path().join("Drums/90bpm_boom.wav"), 90.0);
        let state = IpcState::new();
        let cache = index_cache_path(&dir.path().join("cache"));

        assert!(state.library_scan(&[], &cache, false).is_err());
        let status = state
            .library_scan(&[dir.path().join("Drums")], &cache, false)
            .unwrap();
        assert_eq!(status.entries, 2);
        assert!(!status.scanning && !status.cancelled);
        assert_eq!(status.analysed, 2);

        let all = state.library_search(&LibraryQuery::default(), &cache);
        assert_eq!((all.total, all.matched), (2, 2));
        let slow = state.library_search(
            &LibraryQuery {
                bpm_max: Some(100.0),
                ..LibraryQuery::default()
            },
            &cache,
        );
        assert_eq!(slow.entries.len(), 1);
        assert_eq!(slow.entries[0].name, "90bpm_boom.wav");
        assert_eq!(slow.entries[0].bpm, Some(90.0));
        assert!(!slow.entries[0].bpm_detected, "the tempo is from the name");
        let text = state.library_search(
            &LibraryQuery {
                text: "kick".into(),
                limit: 1,
                ..LibraryQuery::default()
            },
            &cache,
        );
        assert_eq!((text.matched, text.entries.len()), (1, 1));

        // A new session reads the cache before any scan.
        let fresh = IpcState::new();
        assert_eq!(
            fresh.library_search(&LibraryQuery::default(), &cache).total,
            2
        );
        assert!(!state.library_cancel(), "nothing is running");
    }

    fn engine() -> Engine {
        use ow_io::mock::{MockBackend, MockConfig, MockDrive, MockMidiBackend};
        use ow_io::{AudioBackend, AudioSelection, BackendRegistry, MidiBackend, StreamRequest};
        use std::sync::Arc;
        let backend = MockBackend::new(MockConfig {
            drive: MockDrive::Thread { paced: true },
            buffer_frames: 64,
            ..MockConfig::default()
        });
        let registry = BackendRegistry::with(vec![Arc::new(backend) as Arc<dyn AudioBackend>]);
        let config = ow_engine::EngineConfig {
            audio: AudioSelection {
                backend: "mock".into(),
                request: StreamRequest::default(),
            },
            ..ow_engine::EngineConfig::default()
        };
        let midi: Arc<dyn MidiBackend> = Arc::new(MockMidiBackend::new([] as [&str; 0]));
        Engine::with_backends(registry, midi, config)
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn a_preview_plays_a_file_of_the_index_in_the_project_tempo_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Drums/120bpm_kick.wav");
        write_loop(&file, 120.0);
        let state = IpcState::new();
        let engine = engine();
        wait_until("the mock stream", || engine.audio_status().info.is_some());
        let cache = index_cache_path(&dir.path().join("cache"));

        let error = state.library_preview(&engine, &file, 0.0).unwrap_err();
        assert!(error.contains("not in the library"), "{error}");
        state
            .library_scan(&[dir.path().join("Drums")], &cache, false)
            .unwrap();
        let reply = state.library_preview(&engine, &file, 0.0).unwrap();
        // The default project is at 120 bpm: the loop plays at its own speed, 8 s long.
        assert_eq!(reply.source_bpm, Some(120.0));
        assert_eq!(reply.target_bpm, Some(120.0));
        assert!((reply.seconds - 8.0).abs() < 0.01, "{}", reply.seconds);
        assert!(engine.preview_playing());
        state.library_preview_stop(&engine);
        wait_until("the preview to end", || !engine.preview_playing());
    }
}
