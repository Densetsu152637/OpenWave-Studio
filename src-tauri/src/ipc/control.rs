//! The control-thread state behind the IPC commands: the authoritative [`Session`], the patch
//! channel and the hand-off of every edited project to the engine.
//!
//! This file is free of Tauri types so it can be tested without a window: the patch channel and
//! the engine are behind two small traits. The project lives here and nowhere near the audio
//! callback; the engine only ever receives a compiled plan.

use ow_engine::Engine;
use ow_ipc::{CommandReply, ModelPatch, PatchOp, Project, Session, Snapshot, UiCommand};

/// Where patches go (the UI's `ipc::Channel`).
pub trait PatchSink: Send {
    /// Sends one patch; false when the receiver is gone.
    fn send(&self, patch: &ModelPatch) -> bool;
}

/// What an edited project is handed to (the engine).
pub trait PlanTarget {
    /// Compiles and swaps in the plan for `project`.
    fn apply_project(&self, project: &Project) -> Result<(), String>;
}

impl PlanTarget for Engine {
    fn apply_project(&self, project: &Project) -> Result<(), String> {
        self.set_project(project.clone())
            .map(|_| ())
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

    fn apply_to_engine(&mut self, target: &dyn PlanTarget) {
        self.engine_error = match target.apply_project(self.session.project()) {
            Ok(()) => None,
            Err(error) => {
                eprintln!("[openwave] could not apply the project to the engine: {error}");
                Some(error)
            }
        };
        // The UI hears about a failure (and its recovery) as a patch, not on the next command.
        if let Some(patch) = self.session.set_engine_error(self.engine_error.clone()) {
            self.deliver(&patch);
        }
    }

    /// Swaps in another project (a project was opened): one `Reset` patch to the UI and a fresh
    /// plan for the engine.
    pub fn replace_project(&mut self, project: Project, target: &dyn PlanTarget) {
        let patch = self.session.replace_project(project);
        self.deliver(&patch);
        self.apply_to_engine(target);
    }

    fn deliver(&mut self, patch: &ModelPatch) {
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
    }

    impl PlanTarget for Engineish {
        fn apply_project(&self, project: &Project) -> Result<(), String> {
            self.applied.borrow_mut().push(project.name.clone());
            let transient = self.fail_next.get();
            if transient > 0 {
                self.fail_next.set(transient - 1);
                return Err("busy".into());
            }
            if self.fail { Err("no".into()) } else { Ok(()) }
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
        control.replace_project(Project::new("Opened"), &target);
        let patches = sink.0.lock().unwrap().clone();
        assert_eq!(patches.len(), 1);
        assert!(matches!(patches[0].ops[0], PatchOp::Reset { .. }));
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
}
