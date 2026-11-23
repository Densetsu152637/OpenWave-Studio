//! MVP goal M-G2: import, save, open and export work end to end through the control layer,
//! without a window. The plan target compiles every project for real (media loaded, resampled
//! and checked), so the tests also prove the plans the engine would receive are sound.

use std::cell::RefCell;
use std::f64::consts::TAU;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ow_engine::{CompileContext, compile_project};
use ow_ipc::peaks::decode_peaks;
use ow_ipc::{EditCommand, Mirror, MixerRef, ModelPatch, Project, Snapshot, UiCommand};
use ow_model::{Clip, ClipContent, MediaId, Owned, Strip, Track, TrackId, TrackKind};

use super::control::{PatchSink, PlanTarget};
use super::{IpcState, lock};

const RATE: u32 = 48_000;
/// One beat at the default 120 BPM.
const BEAT: usize = 24_000;
const LEFT_HZ: f64 = 220.0;
const RIGHT_HZ: f64 = 330.0;

/// Compiles each project for the project's own rate and records what was missing.
#[derive(Default)]
struct CompilingTarget {
    applied: RefCell<Vec<Vec<u64>>>,
}

impl PlanTarget for CompilingTarget {
    fn apply_project(&self, project: &Project, ctx: &CompileContext) -> Result<Vec<u64>, String> {
        let compiled = compile_project(project, ctx, project.settings.sample_rate, 512)
            .map_err(|error| error.to_string())?;
        let missing: Vec<u64> = compiled.missing_media.iter().map(|id| id.0).collect();
        self.applied.borrow_mut().push(missing.clone());
        Ok(missing)
    }
}

#[derive(Clone, Default)]
pub(super) struct Collect(Arc<Mutex<Vec<ModelPatch>>>);

impl PatchSink for Collect {
    fn send(&self, patch: &ModelPatch) -> bool {
        self.0.lock().unwrap().push(patch.clone());
        true
    }
}

/// A UI: attached to the control, replaying every patch into a mirror.
pub(super) struct Ui {
    sink: Collect,
    pub(super) mirror: Mirror,
    seen: usize,
}

impl Ui {
    pub(super) fn attach(state: &IpcState) -> Self {
        let sink = Collect::default();
        let snapshot = lock(&state.control).attach(Box::new(sink.clone()));
        Self {
            sink,
            mirror: Mirror::from_snapshot(snapshot),
            seen: 0,
        }
    }

    /// Applies the patches that arrived since the last call and checks the mirror equals the
    /// control's state, returning that state.
    pub(super) fn sync(&mut self, state: &IpcState) -> Snapshot {
        let patches = self.sink.0.lock().unwrap().clone();
        for patch in &patches[self.seen..] {
            self.mirror.apply(patch).expect("patches arrive in order");
        }
        self.seen = patches.len();
        let snapshot = lock(&state.control).snapshot();
        assert_eq!(self.mirror.project, snapshot.project);
        assert_eq!(self.mirror.history, snapshot.history);
        assert_eq!(self.mirror.project_path, snapshot.project_path);
        assert_eq!(self.mirror.seq, snapshot.seq);
        snapshot
    }
}

fn left(frame: usize) -> f32 {
    (0.5 * (TAU * LEFT_HZ * frame as f64 / f64::from(RATE)).sin()) as f32
}

fn right(frame: usize) -> f32 {
    (0.25 * (TAU * RIGHT_HZ * frame as f64 / f64::from(RATE)).sin()) as f32
}

/// A two-second stereo 32-bit float WAV of the analytic tones.
fn write_fixture(path: &Path) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for frame in 0..2 * RATE as usize {
        writer.write_sample(left(frame)).unwrap();
        writer.write_sample(right(frame)).unwrap();
    }
    writer.finalize().unwrap();
}

pub(super) fn command(state: &IpcState, target: &dyn PlanTarget, command: UiCommand) -> Vec<u64> {
    lock(&state.control).command(command, target).unwrap().ids
}

pub(super) fn edit(state: &IpcState, target: &dyn PlanTarget, edit: EditCommand) {
    command(state, target, UiCommand::Edit { edit });
}

/// An audio track and a clip of `media` on it: beat 1, two beats long, starting 1000 frames
/// into the file.
fn add_clip(state: &IpcState, target: &dyn PlanTarget, media: u64) {
    let ids = command(state, target, UiCommand::AllocIds { count: 2 });
    let track = TrackId(ids[0]);
    edit(
        state,
        target,
        EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned::new(Track {
                id: track,
                name: "Audio".into(),
                color: 0,
                kind: TrackKind::Audio,
                strip: Strip::default(),
                instrument: None,
            })),
        },
    );
    edit(
        state,
        target,
        EditCommand::AddClip {
            clip: Box::new(Clip {
                id: ids[1].into(),
                track,
                name: "Take".into(),
                start: 1.0,
                length: 2.0,
                muted: false,
                content: ClipContent::Audio {
                    media: MediaId(media),
                    source_offset: 1000,
                    gain_db: 0.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                },
            }),
        },
    );
}

fn read_wav(path: &Path) -> (hound::WavSpec, Vec<f32>) {
    let mut reader = hound::WavReader::open(path).unwrap();
    let spec = reader.spec();
    (spec, reader.samples::<f32>().map(Result::unwrap).collect())
}

fn bundle_files(bundle: &Path, dir: &str) -> Vec<PathBuf> {
    std::fs::read_dir(bundle.join(dir))
        .map(|entries| entries.map(|e| e.unwrap().path()).collect())
        .unwrap_or_default()
}

#[test]
fn mvp_import_save_open_and_export_work_end_to_end() {
    let work = tempfile::tempdir().unwrap();
    let wav = work.path().join("take one.wav");
    write_fixture(&wav);
    let target = CompilingTarget::default();
    let state = IpcState::new();
    let mut ui = Ui::attach(&state);

    // A new project is unsaved and clean.
    let snapshot = ui.sync(&state);
    assert_eq!(snapshot.project_path, None);
    assert!(!snapshot.history.dirty);

    // Import into the unsaved project: the media lands in a temporary folder.
    let reply = state.media_import(&wav, &target).unwrap();
    assert_eq!(
        (reply.frames, reply.sample_rate, reply.channels),
        (2 * u64::from(RATE), RATE, 2)
    );
    let temp_root = lock(&state.control).media_root().to_path_buf();
    assert!(temp_root.join("audio").is_dir());
    let snapshot = ui.sync(&state);
    assert_eq!(snapshot.project.media_pool.len(), 1);
    let item = snapshot.project.media_pool.values().next().unwrap().clone();
    assert_eq!(item.id.0, reply.media);
    assert_eq!(item.name, "take one");
    assert!(temp_root.join(&item.path).is_file());
    // The peaks are served right away.
    let bytes = state.peaks.range(reply.media, 0, 2 * u64::from(RATE), 100);
    let peaks = decode_peaks(&bytes).unwrap();
    assert_eq!(peaks.len(), 100);
    assert!(peaks.iter().any(|(_, hi)| *hi > 0.4));
    // Importing the same file again finds the item instead of adding another.
    assert_eq!(state.media_import(&wav, &target).unwrap(), reply);
    assert_eq!(ui.sync(&state).project.media_pool.len(), 1);

    // Arrange it; the project is dirty.
    add_clip(&state, &target, reply.media);
    let snapshot = ui.sync(&state);
    assert!(snapshot.history.dirty);
    assert!(snapshot.history.missing_media.is_empty());
    assert_eq!(snapshot.project.clips.len(), 1);

    // Saving without a path has nowhere to go.
    assert!(state.project_save(None, &target).is_err());
    assert!(ui.sync(&state).history.dirty);

    // Save As: media is copied (not moved) into the bundle, the temporary folder goes away.
    let saved = state
        .project_save(Some(&work.path().join("song")), &target)
        .unwrap();
    let bundle = PathBuf::from(&saved);
    assert_eq!(bundle, work.path().join("song.owave"));
    assert!(bundle.join("project.json").is_file());
    assert!(bundle.join(&item.path).is_file());
    assert!(!temp_root.exists(), "the temporary media folder is deleted");
    assert_eq!(lock(&state.control).media_root(), bundle);
    let snapshot = ui.sync(&state);
    assert_eq!(snapshot.project_path.as_deref(), Some(saved.as_str()));
    assert!(!snapshot.history.dirty, "saving clears the dirty flag");
    assert!(snapshot.history.missing_media.is_empty());
    state.wait_for_peaks();
    assert!(
        bundle
            .join("cache")
            .join(format!("{}.owpk", item.hash))
            .is_file()
    );

    // An edit dirties it again; a plain save writes the same bundle and cleans it.
    edit(
        &state,
        &target,
        EditCommand::SetName {
            name: "Song".into(),
        },
    );
    assert!(ui.sync(&state).history.dirty);
    assert_eq!(state.project_save(None, &target).unwrap(), saved);
    assert!(!ui.sync(&state).history.dirty);
    assert_eq!(bundle_files(&bundle, "audio").len(), 1);

    // Open it in a fresh control (peaks missing on disk: they are rebuilt).
    std::fs::remove_file(bundle.join("cache").join(format!("{}.owpk", item.hash))).unwrap();
    let fresh = IpcState::new();
    let mut fresh_ui = Ui::attach(&fresh);
    let opened = fresh.project_open(&bundle, &target).unwrap();
    assert_eq!(opened.project, ui.mirror.project);
    assert_eq!(opened.project_path.as_deref(), Some(saved.as_str()));
    assert!(!opened.history.dirty);
    assert!(opened.history.missing_media.is_empty());
    assert_eq!(fresh_ui.sync(&fresh), opened);
    fresh.wait_for_peaks();
    let bytes = fresh.peaks.range(reply.media, 0, 2 * u64::from(RATE), 100);
    assert!(
        decode_peaks(&bytes)
            .unwrap()
            .iter()
            .any(|(_, hi)| *hi > 0.4),
        "the opened project's waveform is served"
    );
    edit(
        &fresh,
        &target,
        EditCommand::SetName {
            name: "Edited".into(),
        },
    );
    assert!(fresh_ui.sync(&fresh).history.dirty);

    // Export: the clip's beat-1 to beat-3 window of the file, then a one-second tail.
    let exported = fresh.export_wav(&work.path().join("mix")).unwrap();
    assert_eq!(PathBuf::from(&exported), work.path().join("mix.wav"));
    let (spec, samples) = read_wav(Path::new(&exported));
    assert_eq!((spec.channels, spec.sample_rate), (2, RATE));
    assert_eq!(spec.sample_format, hound::SampleFormat::Float);
    let frames = 3 * BEAT + RATE as usize;
    assert_eq!(samples.len(), frames * 2, "last clip end plus one second");
    let mut worst = 0.0_f64;
    for frame in 0..frames {
        let (want_left, want_right) = if (BEAT..3 * BEAT).contains(&frame) {
            let source = 1000 + frame - BEAT;
            (left(source), right(source))
        } else {
            (0.0, 0.0)
        };
        worst = worst
            .max(f64::from((samples[frame * 2] - want_left).abs()))
            .max(f64::from((samples[frame * 2 + 1] - want_right).abs()));
    }
    assert!(
        worst < 1e-5,
        "export differs from the analytic mix by {worst}"
    );
    assert!(
        samples[BEAT * 2..BEAT * 2 + 2000]
            .iter()
            .any(|s| s.abs() > 0.1)
    );
    // The export did not touch the project.
    assert!(fresh_ui.sync(&fresh).history.dirty);
}

#[test]
fn mvp_saving_elsewhere_copies_media_and_keeps_the_old_bundle() {
    let work = tempfile::tempdir().unwrap();
    let wav = work.path().join("a.wav");
    write_fixture(&wav);
    let target = CompilingTarget::default();
    let state = IpcState::new();
    let reply = state.media_import(&wav, &target).unwrap();
    add_clip(&state, &target, reply.media);
    let first = PathBuf::from(
        state
            .project_save(Some(&work.path().join("one.owave")), &target)
            .unwrap(),
    );
    let second = PathBuf::from(
        state
            .project_save(Some(&work.path().join("two.owave")), &target)
            .unwrap(),
    );
    // Save As leaves the first bundle complete and moves the project to the second.
    assert_eq!(bundle_files(&first, "audio").len(), 1);
    assert_eq!(bundle_files(&second, "audio").len(), 1);
    assert_eq!(lock(&state.control).media_root(), second);
    assert_eq!(
        lock(&state.control).session().project_path(),
        second.to_str()
    );
    state.wait_for_peaks();
    assert!(state.export_wav(&work.path().join("x.wav")).is_ok());
}

#[test]
fn mvp_missing_media_is_a_warning_and_new_starts_clean() {
    let work = tempfile::tempdir().unwrap();
    let wav = work.path().join("a.wav");
    write_fixture(&wav);
    let target = CompilingTarget::default();
    let state = IpcState::new();
    let reply = state.media_import(&wav, &target).unwrap();
    add_clip(&state, &target, reply.media);
    let bundle = PathBuf::from(
        state
            .project_save(Some(&work.path().join("song")), &target)
            .unwrap(),
    );
    let file = bundle_files(&bundle, "audio").remove(0);
    std::fs::remove_file(file).unwrap();

    // Opening a bundle whose media file is gone works and lists the media as missing.
    let fresh = IpcState::new();
    let mut ui = Ui::attach(&fresh);
    let opened = fresh.project_open(&bundle, &target).unwrap();
    assert_eq!(opened.history.missing_media, [reply.media]);
    assert_eq!(opened.history.engine_error, None);
    assert_eq!(ui.sync(&fresh), opened);
    // Exporting still works: the clip is silent.
    let out = fresh.export_wav(&work.path().join("silent.wav")).unwrap();
    let (_, samples) = read_wav(Path::new(&out));
    assert_eq!(samples.len(), (3 * BEAT + RATE as usize) * 2);
    assert!(samples.iter().all(|s| *s == 0.0));

    // A bundle that does not exist is refused and changes nothing.
    let seq = ui.sync(&fresh).seq;
    assert!(
        fresh
            .project_open(&work.path().join("nope.owave"), &target)
            .is_err()
    );
    assert_eq!(ui.sync(&fresh).seq, seq);

    // New: an empty, clean, unsaved project.
    let snapshot = fresh.project_new(&target).unwrap();
    assert_eq!(snapshot.project_path, None);
    assert!(!snapshot.history.dirty);
    assert!(snapshot.history.missing_media.is_empty());
    assert!(snapshot.project.clips.is_empty());
    assert_eq!(ui.sync(&fresh), snapshot);
    assert_eq!(lock(&fresh.control).media_root(), Path::new(""));
    // The master strip is still there, so the empty project compiles and exports a second.
    assert!(snapshot.project.strip(MixerRef::Master).is_some());
    let out = fresh.export_wav(&work.path().join("empty.wav")).unwrap();
    assert_eq!(read_wav(Path::new(&out)).1.len(), RATE as usize * 2);
}
