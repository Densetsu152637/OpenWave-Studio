//! The control-thread side of the state mirror: the authoritative [`Editor`] plus the patch
//! sequence.
//!
//! A [`Session`] turns each [`UiCommand`] into at most one [`ModelPatch`], numbered one higher
//! than the last. The caller (the Tauri layer) sends the patch to the UI in the order
//! [`handle`](Session::handle) returns them, which is what keeps the stream ordered. A UI that
//! sees a gap (or just attached) asks for a [`snapshot`](Session::snapshot) and continues from
//! its `seq`.

use ow_model::{EditError, Editor, Project};

use crate::dto::{HistoryState, MAX_ALLOC_IDS, ModelPatch, PatchOp, Snapshot, UiCommand};
use crate::mirror::diff;

/// What a command did.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// The patch to send to the UI, when the model or the undo state changed. A refused command
    /// changes nothing and yields none, except after a corrupt history (which discards it).
    pub patch: Option<ModelPatch>,
    /// Why the command was refused, if it was.
    pub error: Option<EditError>,
    /// Ids reserved by [`UiCommand::AllocIds`].
    pub ids: Vec<u64>,
}

/// The project, its history and the patch counter.
#[derive(Debug)]
pub struct Session {
    editor: Editor,
    seq: u64,
    history: HistoryState,
    engine_error: Option<String>,
}

impl Session {
    /// A session over `project`, at sequence 0.
    pub fn new(project: Project) -> Self {
        let editor = Editor::new(project);
        let history = history_of(&editor, None);
        Self {
            editor,
            seq: 0,
            history,
            engine_error: None,
        }
    }

    /// Sequence number of the latest patch.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// The editor (read-only).
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// The project (read-only).
    pub fn project(&self) -> &Project {
        self.editor.project()
    }

    /// The whole state, for a UI that attaches or resynchronizes.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            seq: self.seq,
            project: self.editor.project().clone(),
            history: self.history.clone(),
        }
    }

    /// Records that the current state is what is on disk, and reports the dirty flag change.
    pub fn mark_saved(&mut self) -> Option<ModelPatch> {
        self.editor.mark_saved();
        self.finish(Vec::new())
    }

    /// Records whether the engine plays the current project (`None`) or why it does not. Yields a
    /// patch (no operations, new history) only when the state changed, so the UI hears about a
    /// failed swap right away and about its recovery.
    pub fn set_engine_error(&mut self, error: Option<String>) -> Option<ModelPatch> {
        if self.engine_error == error {
            return None;
        }
        self.engine_error = error;
        self.finish(Vec::new())
    }

    /// Swaps in another project (one was opened): the history is discarded and the UI receives
    /// one `Reset` patch.
    pub fn replace_project(&mut self, project: Project) -> ModelPatch {
        self.editor = Editor::new(project);
        self.seq += 1;
        self.history = history_of(&self.editor, self.engine_error.clone());
        ModelPatch {
            seq: self.seq,
            ops: vec![PatchOp::Reset {
                project: Box::new(self.editor.project().clone()),
            }],
            history: self.history.clone(),
        }
    }

    /// Runs one command.
    pub fn handle(&mut self, command: UiCommand) -> Outcome {
        let mutates = !matches!(command, UiCommand::Begin { .. } | UiCommand::End);
        let mut ids = Vec::new();
        let before = mutates.then(|| self.editor.project().clone());
        let revision = self.editor.revision();
        let result = match command {
            UiCommand::Edit { edit } => self.editor.apply(edit),
            UiCommand::Undo => self.editor.undo(),
            UiCommand::Redo => self.editor.redo(),
            UiCommand::Begin { label } => {
                self.editor.begin(label);
                Ok(())
            }
            UiCommand::End => self.editor.end(),
            UiCommand::Cancel => self.editor.cancel(),
            UiCommand::AllocIds { count } => self.alloc_ids(count, &mut ids),
        };
        // A refused command normally changes nothing; `cancel` and a corrupt history can change
        // the project before failing, so the diff is taken either way.
        let ops = match &before {
            Some(before) if self.editor.revision() != revision => {
                diff(before, self.editor.project())
            }
            _ => Vec::new(),
        };
        // Reserving ids moves the counter without an edit, so it is diffed separately.
        let ops = if ops.is_empty() && !ids.is_empty() {
            vec![PatchOp::SetNextId {
                next_id: self.editor.project().next_id,
            }]
        } else {
            ops
        };
        Outcome {
            patch: self.finish(ops),
            error: result.err(),
            ids,
        }
    }

    fn alloc_ids(&mut self, count: u32, ids: &mut Vec<u64>) -> Result<(), EditError> {
        if count > MAX_ALLOC_IDS {
            return Err(EditError::WrongTarget("too many ids requested"));
        }
        for _ in 0..count {
            match self.editor.alloc_id::<u64>() {
                Ok(id) => ids.push(id),
                Err(_) => {
                    return Err(EditError::WrongTarget("the project has run out of ids"));
                }
            }
        }
        Ok(())
    }

    /// Wraps `ops` into the next patch when there is anything to report.
    fn finish(&mut self, ops: Vec<PatchOp>) -> Option<ModelPatch> {
        let history = history_of(&self.editor, self.engine_error.clone());
        if ops.is_empty() && history == self.history {
            return None;
        }
        self.seq += 1;
        self.history = history;
        Some(ModelPatch {
            seq: self.seq,
            ops,
            history: self.history.clone(),
        })
    }
}

fn history_of(editor: &Editor, engine_error: Option<String>) -> HistoryState {
    HistoryState {
        can_undo: editor.can_undo(),
        can_redo: editor.can_redo(),
        undo_label: editor.undo_label().map(str::to_owned),
        redo_label: editor.redo_label().map(str::to_owned),
        in_transaction: editor.in_transaction(),
        dirty: editor.is_dirty(),
        engine_error,
    }
}
