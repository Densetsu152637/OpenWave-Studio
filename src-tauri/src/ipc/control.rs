//! The control-thread state behind the IPC commands: the authoritative [`Session`], the patch
//! channel and the hand-off of every edited project to the engine.
//!
//! This file is free of Tauri types so it can be tested without a window: the patch channel and
//! the engine are behind two small traits. The project lives here and nowhere near the audio
//! callback; the engine only ever receives a compiled plan.
//!
//! The control also owns where the project's media lives ([`CompileContext`]): the `.owave`
//! bundle once the project is saved or opened, a per-session temporary folder before that
//! (created by the first import, deleted when the project is saved elsewhere, replaced or the
//! app exits).

use std::path::{Path, PathBuf};

use ow_engine::{CompileContext, Engine};
use ow_ipc::{CommandReply, ModelPatch, PatchOp, Project, Session, Snapshot, UiCommand};
use ow_model::{TrackId, TrackKind};

/// Where patches go (the UI's `ipc::Channel`).
pub trait PatchSink: Send {
    /// Sends one patch; false when the receiver is gone.
    fn send(&self, patch: &ModelPatch) -> bool;
}

/// What an edited project is handed to (the engine).
pub trait PlanTarget {
    /// Compiles and swaps in the plan for `project`, loading its media through `ctx`. Returns
    /// the ids of the media items whose files could not be loaded (their clips are silent).
    fn apply_project(&self, project: &Project, ctx: &CompileContext) -> Result<Vec<u64>, String>;
}

impl PlanTarget for Engine {
    fn apply_project(&self, project: &Project, ctx: &CompileContext) -> Result<Vec<u64>, String> {
        self.set_project(project.clone(), ctx.clone())
            .map(|swap| swap.missing_media.iter().map(|id| id.0).collect())
            .map_err(|error| error.to_string())
    }
}

/// The session plus its single UI subscriber.
pub struct Control {
    session: Session,
    sink: Option<Box<dyn PatchSink>>,
    /// The engine does not play the current project (the last swap failed); retried by the next
    /// command, whatever it is.
    engine_error: Option<String>,
    /// Where media is read from, and the media already loaded.
    pub(super) ctx: CompileContext,
    /// The temporary media folder of a project that was never saved.
    pub(super) temp: Option<tempfile::TempDir>,
    /// Counts the projects this control has swapped in (open, new), so a recording can tell
    /// that the project it started in is gone.
    epoch: u64,
}

/// True when an operation changes what the engine plays; renaming the project or reserving ids
/// does not, so those skip the recompile.
fn affects_audio(op: &PatchOp) -> bool {
    !matches!(op, PatchOp::SetName { .. } | PatchOp::SetNextId { .. })
}

impl Control {
    /// A control state over an empty project.
    pub fn new() -> Self {
        Self::with_project(Project::new("Untitled"))
    }

    /// A control state over `project` (the engine is not told until the first edit).
    pub fn with_project(project: Project) -> Self {
        Self {
            session: Session::new(project),
            sink: None,
            engine_error: None,
            ctx: CompileContext::default(),
            temp: None,
            epoch: 0,
        }
    }

    /// Changes whenever another project replaces the current one.
    pub(super) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Arms or disarms an audio track for recording. Session state: no undo step, no dirty
    /// flag; the UI sees it through the mirror's `history.armedTracks`.
    pub(super) fn set_armed(&mut self, track: u64, armed: bool) -> Result<(), String> {
        let is_audio = self
            .session
            .project()
            .tracks
            .iter()
            .any(|t| t.id == TrackId(track) && t.kind == TrackKind::Audio);
        if armed && !is_audio {
            return Err("only an audio track can be armed for recording".into());
        }
        let mut next: Vec<u64> = self.session.armed_tracks().to_vec();
        next.retain(|id| *id != track);
        if armed {
            next.push(track);
            next.sort_unstable();
        }
        if let Some(patch) = self.session.set_armed_tracks(next) {
            self.deliver(&patch);
        }
        Ok(())
    }

    /// The armed tracks that still exist as audio tracks, in project order.
    pub(super) fn armed_audio_tracks(&self) -> Vec<TrackId> {
        let armed = self.session.armed_tracks();
        self.session
            .project()
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio && armed.contains(&t.id.0))
            .map(|t| t.id)
            .collect()
    }

    /// Disarms tracks an edit removed (or turned into something that cannot record).
    fn prune_armed(&mut self) {
        let live: Vec<u64> = self
            .session
            .project()
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .map(|t| t.id.0)
            .collect();
        let kept: Vec<u64> = self
            .session
            .armed_tracks()
            .iter()
            .copied()
            .filter(|id| live.contains(id))
            .collect();
        if let Some(patch) = self.session.set_armed_tracks(kept) {
            self.deliver(&patch);
        }
    }

    /// The folder media paths are relative to (empty until the first import of an unsaved
    /// project).
    pub fn media_root(&self) -> &Path {
        &self.ctx.media_root
    }

    /// The media root for an import: the temporary folder is created on first use while the
    /// project has no file.
    pub(super) fn media_root_for_import(&mut self) -> Result<PathBuf, String> {
        if self.session.project_path().is_none() && self.temp.is_none() {
            let dir = tempfile::Builder::new()
                .prefix("openwave-media-")
                .tempdir()
                .map_err(|error| format!("could not create a temporary media folder: {error}"))?;
            self.ctx.media_root = dir.path().to_path_buf();
            self.temp = Some(dir);
        }
        Ok(self.ctx.media_root.clone())
    }

    /// Records where the project is saved and tells the UI.
    pub(super) fn set_project_path(&mut self, path: Option<String>) {
        if let Some(patch) = self.session.set_project_path(path) {
            self.deliver(&patch);
        }
    }

    /// Records that the project equals what is on disk and tells the UI.
    pub(super) fn mark_saved(&mut self) {
        if let Some(patch) = self.session.mark_saved() {
            self.deliver(&patch);
        }
    }

    /// The session (read-only).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Registers the UI's patch channel (replacing an earlier one) and returns the snapshot it
    /// continues from. Both happen under the caller's lock, so no patch falls in between.
    pub fn attach(&mut self, sink: Box<dyn PatchSink>) -> Snapshot {
        self.sink = Some(sink);
        self.session.snapshot()
    }

    /// The current state, for a UI that saw a gap.
    pub fn snapshot(&self) -> Snapshot {
        self.session.snapshot()
    }

    /// Runs one UI command: applies it, sends the resulting patch (in order, under the caller's
    /// lock) and, when the edit changed what is played, recompiles and swaps the plan. A refused
    /// command comes back as the error text; a plan that fails to compile is logged and the
    /// previous plan keeps playing.
    pub fn command(
        &mut self,
        command: UiCommand,
        target: &dyn PlanTarget,
    ) -> Result<CommandReply, String> {
        let outcome = self.session.handle(command);
        let changed = outcome
            .patch
            .as_ref()
            .is_some_and(|patch| patch.ops.iter().any(affects_audio));
        if let Some(patch) = &outcome.patch {
            self.deliver(patch);
        }
        if !self.session.armed_tracks().is_empty() {
            self.prune_armed();
        }
        if changed || self.engine_error.is_some() {
            self.apply_to_engine(target);
        }
        match outcome.error {
            Some(error) => Err(error.to_string()),
            None => Ok(CommandReply {
                seq: self.session.seq(),
                ids: outcome.ids,
                engine_error: self.engine_error.clone(),
            }),
        }
    }

    /// Why the engine is not playing the current project, if it is not.
    pub fn engine_error(&self) -> Option<&str> {
        self.engine_error.as_deref()
    }

    /// Compiles the current project for the engine against the current media root. A failure is
    /// kept (and reported to the UI) and the engine keeps its previous plan; the list of media
    /// the plan could not load is reported as a warning.
    pub(super) fn apply_to_engine(&mut self, target: &dyn PlanTarget) {
        match target.apply_project(self.session.project(), &self.ctx) {
            Ok(missing) => {
                self.engine_error = None;
                self.deliver_status(missing);
            }
            Err(error) => {
                eprintln!("[openwave] could not apply the project to the engine: {error}");
                self.engine_error = Some(error);
                self.deliver_status(self.session.snapshot().history.missing_media);
            }
        }
    }

    /// Sends the patches for a changed engine error and missing-media list (the UI hears about
    /// a failure, its recovery and missing files as patches, not on the next command).
    fn deliver_status(&mut self, missing: Vec<u64>) {
        if let Some(patch) = self.session.set_engine_error(self.engine_error.clone()) {
            self.deliver(&patch);
        }
        if let Some(patch) = self.session.set_missing_media(missing) {
            self.deliver(&patch);
        }
    }

    /// Swaps in another project (one was opened or created, saved at `project_path` when it
    /// came from a file): one patch to the UI and a fresh plan for the engine. The caller has
    /// already pointed the media root at the project's media.
    pub fn replace_project(
        &mut self,
        project: Project,
        project_path: Option<String>,
        target: &dyn PlanTarget,
    ) {
        let patch = self.session.replace_project(project, project_path);
        self.epoch += 1;
        self.deliver(&patch);
        self.apply_to_engine(target);
    }

    pub(super) fn deliver(&mut self, patch: &ModelPatch) {
        if let Some(sink) = &self.sink
            && !sink.send(patch)
        {
            // The webview went away (reload): the next attach starts from a snapshot.
            self.sink = None;
        }
    }
}

impl Default for Control {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};

    use ow_ipc::{EditCommand, Mirror};

    use super::*;

    #[derive(Clone, Default)]
    struct Collect(Arc<Mutex<Vec<ModelPatch>>>);

    impl PatchSink for Collect {
        fn send(&self, patch: &ModelPatch) -> bool {
            self.0.lock().unwrap().push(patch.clone());
            true
        }
    }

    #[derive(Default)]
    struct Engineish {
        applied: RefCell<Vec<String>>,
        fail: bool,
        fail_next: std::cell::Cell<u32>,
        /// What the next successful swap reports as missing media.
        missing: RefCell<Vec<u64>>,
    }

    impl PlanTarget for Engineish {
        fn apply_project(
            &self,
            project: &Project,
            _ctx: &CompileContext,
        ) -> Result<Vec<u64>, String> {
            self.applied.borrow_mut().push(project.name.clone());
            let transient = self.fail_next.get();
            if transient > 0 {
                self.fail_next.set(transient - 1);
                return Err("busy".into());
            }
            if self.fail {
                Err("no".into())
            } else {
                Ok(self.missing.borrow().clone())
            }
        }
    }

    fn rename(name: &str) -> UiCommand {
        UiCommand::Edit {
            edit: EditCommand::SetName { name: name.into() },
        }
    }

    fn set_tempo(bpm: f64) -> UiCommand {
        UiCommand::Edit {
            edit: EditCommand::SetTempoMap {
                tempo_map: ow_model::time::TempoMap::constant(bpm).unwrap(),
            },
        }
    }

    #[test]
    fn attach_returns_a_snapshot_and_patches_follow_in_order() {
        let mut control = Control::new();
        let target = Engineish::default();
        control.command(rename("before"), &target).unwrap();
        let sink = Collect::default();
        let snapshot = control.attach(Box::new(sink.clone()));
        assert_eq!(snapshot.seq, 1);
        let mut mirror = Mirror::from_snapshot(snapshot);

        control.command(set_tempo(90.0), &target).unwrap();
        control.command(UiCommand::Undo, &target).unwrap();
        control.command(UiCommand::Redo, &target).unwrap();
        let patches = sink.0.lock().unwrap().clone();
        assert_eq!(patches.iter().map(|p| p.seq).collect::<Vec<_>>(), [2, 3, 4]);
        for patch in &patches {
            mirror.apply(patch).unwrap();
        }
        assert_eq!(mirror.project, *control.session().project());
    }

    #[test]
    fn only_edits_that_change_the_sound_recompile_the_plan() {
        let mut control = Control::new();
        let target = Engineish::default();
        control.command(rename("a"), &target).unwrap();
        control
            .command(UiCommand::AllocIds { count: 2 }, &target)
            .unwrap();
        assert!(
            target.applied.borrow().is_empty(),
            "a rename and ids do not recompile"
        );
        control.command(set_tempo(100.0), &target).unwrap();
        control.command(UiCommand::Undo, &target).unwrap();
        assert_eq!(
            target.applied.borrow().len(),
            2,
            "an edit and its undo each recompile"
        );
    }

    #[test]
    fn a_refused_command_reports_the_error_and_sends_nothing() {
        let mut control = Control::new();
        let target = Engineish::default();
        let sink = Collect::default();
        control.attach(Box::new(sink.clone()));
        let error = control.command(UiCommand::Undo, &target).unwrap_err();
        assert!(error.contains("nothing to undo"), "{error}");
        assert!(sink.0.lock().unwrap().is_empty());
        assert_eq!(control.session().seq(), 0);
    }

    #[test]
    fn alloc_ids_replies_with_fresh_ids() {
        let mut control = Control::new();
        let target = Engineish::default();
        let reply = control
            .command(UiCommand::AllocIds { count: 3 }, &target)
            .unwrap();
        assert_eq!(reply.ids.len(), 3);
        assert!(reply.ids.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(reply.seq, 1);
        let again = control
            .command(UiCommand::AllocIds { count: 1 }, &target)
            .unwrap();
        assert!(again.ids[0] > reply.ids[2], "ids are never reused");
    }

    #[test]
    fn a_failing_plan_does_not_fail_the_edit() {
        let mut control = Control::new();
        let target = Engineish {
            fail: true,
            ..Engineish::default()
        };
        assert!(control.command(set_tempo(130.0), &target).is_ok());
        // The edit's patch, then the patch that reports the engine failure.
        assert_eq!(control.session().seq(), 2);
    }

    #[test]
    fn replace_project_sends_a_reset_and_recompiles() {
        let mut control = Control::new();
        let target = Engineish::default();
        let sink = Collect::default();
        control.attach(Box::new(sink.clone()));
        control.replace_project(Project::new("Opened"), Some("a.owave".into()), &target);
        let patches = sink.0.lock().unwrap().clone();
        assert_eq!(patches.len(), 1);
        assert!(matches!(patches[0].ops[0], PatchOp::Reset { .. }));
        assert_eq!(
            patches[0].ops[1],
            PatchOp::SetProjectPath {
                project_path: Some("a.owave".into())
            }
        );
        assert_eq!(target.applied.borrow().as_slice(), ["Opened"]);
    }

    #[test]
    fn a_failed_swap_is_reported_and_retried_by_the_next_command() {
        let mut control = Control::new();
        let target = Engineish::default();
        target.fail_next.set(1);
        let sink = Collect::default();
        let snapshot = control.attach(Box::new(sink.clone()));
        let reply = control.command(set_tempo(95.0), &target).unwrap();
        assert_eq!(reply.engine_error.as_deref(), Some("busy"));
        assert_eq!(control.engine_error(), Some("busy"));
        // A rename does not affect the sound, but the pending failure is retried anyway.
        let reply = control.command(rename("x"), &target).unwrap();
        assert_eq!(reply.engine_error, None);
        let errors: Vec<Option<String>> = sink
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.history.engine_error.clone())
            .collect();
        // Edit patch, failure patch, rename patch, recovery patch.
        assert_eq!(
            errors,
            [None, Some("busy".into()), Some("busy".into()), None]
        );
        let mut mirror = Mirror::from_snapshot(snapshot);
        for patch in sink.0.lock().unwrap().iter() {
            mirror.apply(patch).unwrap();
        }
        assert_eq!(mirror.history.engine_error, None);
        assert_eq!(target.applied.borrow().len(), 2);
        assert_eq!(control.engine_error(), None);
    }

    #[test]
    fn missing_media_is_reported_as_a_warning_and_clears_when_it_is_found() {
        let mut control = Control::new();
        let target = Engineish::default();
        let sink = Collect::default();
        control.attach(Box::new(sink.clone()));
        *target.missing.borrow_mut() = vec![4, 9];
        let reply = control.command(set_tempo(100.0), &target).unwrap();
        // The swap succeeded: a warning, not an engine error.
        assert_eq!(reply.engine_error, None);
        assert_eq!(control.session().snapshot().history.missing_media, [4, 9]);
        // Same list again: no extra patch.
        let seq = control.session().seq();
        control.command(set_tempo(101.0), &target).unwrap();
        assert_eq!(control.session().seq(), seq + 1);
        // Found again.
        target.missing.borrow_mut().clear();
        control.command(set_tempo(102.0), &target).unwrap();
        assert!(
            control
                .session()
                .snapshot()
                .history
                .missing_media
                .is_empty()
        );
        let patches = sink.0.lock().unwrap().clone();
        assert_eq!(
            patches.len(),
            5,
            "edit, warning, edit, edit, cleared warning"
        );
    }
}
