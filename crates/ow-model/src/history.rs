//! The editor: the authoritative project plus its undo and redo stacks.

use crate::command::{EditCommand, EditError};
use crate::ids::{BusId, ClipId, DeviceId, TrackId};
use crate::model::Project;

/// Undo steps kept; older ones are dropped.
pub const DEFAULT_HISTORY_LIMIT: usize = 1000;

#[derive(Debug, Clone)]
struct Step {
    label: String,
    /// Applying it reverses the step (and yields the command that redoes it).
    inverse: EditCommand,
}

#[derive(Debug)]
struct Transaction {
    label: String,
    depth: u32,
    /// Inverses of the commands applied so far, oldest first.
    inverses: Vec<EditCommand>,
}

/// The control thread's handle on the project: applies [`EditCommand`]s, keeps undo and redo,
/// and groups gestures into transactions.
///
/// A drag is `begin("Move clip")`, many `apply`s, `end()`: one undo step. `begin` nests; only the
/// outermost `end` closes the step. While a transaction is open undo and redo are refused.
#[derive(Debug)]
pub struct Editor {
    project: Project,
    undo: Vec<Step>,
    redo: Vec<Step>,
    open: Option<Transaction>,
    limit: usize,
    revision: u64,
    saved_revision: u64,
}

impl Editor {
    /// An editor over `project` with an empty history.
    pub fn new(project: Project) -> Self {
        Self {
            project,
            undo: Vec::new(),
            redo: Vec::new(),
            open: None,
            limit: DEFAULT_HISTORY_LIMIT,
            revision: 0,
            saved_revision: 0,
        }
    }

    /// Sets how many undo steps are kept (at least 1).
    pub fn set_history_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
        self.trim();
    }

    /// The project (read-only: all changes go through [`apply`](Self::apply)).
    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Gives the project back, discarding the history.
    pub fn into_project(self) -> Project {
        self.project
    }

    /// A fresh track id.
    pub fn alloc_track_id(&mut self) -> TrackId {
        self.project.alloc_id()
    }

    /// A fresh bus id.
    pub fn alloc_bus_id(&mut self) -> BusId {
        self.project.alloc_id()
    }

    /// A fresh clip id.
    pub fn alloc_clip_id(&mut self) -> ClipId {
        self.project.alloc_id()
    }

    /// A fresh device id.
    pub fn alloc_device_id(&mut self) -> DeviceId {
        self.project.alloc_id()
    }

    /// A fresh id of any kind.
    pub fn alloc_id<I: From<u64>>(&mut self) -> I {
        self.project.alloc_id()
    }

    /// Counts every applied change, undo and redo; changes whenever the project does.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// True when the project changed since [`mark_saved`](Self::mark_saved) (or creation).
    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    /// Records that the current state is what is on disk.
    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }

    /// Applies a command. Inside a transaction its inverse joins the transaction; otherwise it
    /// becomes one undo step and clears the redo stack. On error nothing changed.
    pub fn apply(&mut self, command: EditCommand) -> Result<(), EditError> {
        let label = command.label();
        let inverse = self.project.apply(command)?;
        self.revision += 1;
        match &mut self.open {
            Some(transaction) => transaction.inverses.push(inverse),
            None => self.push_step(Step {
                label: label.to_owned(),
                inverse,
            }),
        }
        Ok(())
    }

    /// Opens a transaction (or deepens the open one); its commands undo as one step named
    /// `label`.
    pub fn begin(&mut self, label: impl Into<String>) {
        match &mut self.open {
            Some(transaction) => transaction.depth += 1,
            None => {
                self.open = Some(Transaction {
                    label: label.into(),
                    depth: 1,
                    inverses: Vec::new(),
                });
            }
        }
    }

    /// Closes the innermost transaction; the outermost close pushes one undo step (nothing if
    /// no command was applied).
    pub fn end(&mut self) -> Result<(), EditError> {
        let transaction = self.open.as_mut().ok_or(EditError::NoTransaction)?;
        transaction.depth -= 1;
        if transaction.depth > 0 {
            return Ok(());
        }
        let Transaction {
            label,
            mut inverses,
            ..
        } = self.open.take().expect("checked above");
        if !inverses.is_empty() {
            inverses.reverse();
            self.push_step(Step {
                label,
                inverse: EditCommand::Batch { commands: inverses },
            });
        }
        Ok(())
    }

    /// Abandons the open transaction, reverting everything applied inside it.
    pub fn cancel(&mut self) -> Result<(), EditError> {
        let transaction = self.open.take().ok_or(EditError::NoTransaction)?;
        for inverse in transaction.inverses.into_iter().rev() {
            self.revision += 1;
            if let Err(error) = self.project.apply(inverse) {
                self.discard_history();
                return Err(EditError::Corrupt(error.to_string()));
            }
        }
        Ok(())
    }

    /// True while a transaction is open.
    pub fn in_transaction(&self) -> bool {
        self.open.is_some()
    }

    /// True when an undo step exists.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// True when a redo step exists.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Label of the step [`undo`](Self::undo) would revert.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    /// Label of the step [`redo`](Self::redo) would reapply.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// Reverts the latest step.
    pub fn undo(&mut self) -> Result<(), EditError> {
        if self.open.is_some() {
            return Err(EditError::TransactionOpen);
        }
        let step = self.undo.pop().ok_or(EditError::NothingToUndo)?;
        match self.project.apply(step.inverse) {
            Ok(redo) => {
                self.revision += 1;
                self.redo.push(Step {
                    label: step.label,
                    inverse: redo,
                });
                Ok(())
            }
            Err(error) => {
                self.discard_history();
                Err(EditError::Corrupt(error.to_string()))
            }
        }
    }

    /// Reapplies the latest undone step.
    pub fn redo(&mut self) -> Result<(), EditError> {
        if self.open.is_some() {
            return Err(EditError::TransactionOpen);
        }
        let step = self.redo.pop().ok_or(EditError::NothingToRedo)?;
        match self.project.apply(step.inverse) {
            Ok(undo) => {
                self.revision += 1;
                self.undo.push(Step {
                    label: step.label,
                    inverse: undo,
                });
                Ok(())
            }
            Err(error) => {
                self.discard_history();
                Err(EditError::Corrupt(error.to_string()))
            }
        }
    }

    fn push_step(&mut self, step: Step) {
        self.redo.clear();
        self.undo.push(step);
        self.trim();
    }

    fn trim(&mut self) {
        if self.undo.len() > self.limit {
            let excess = self.undo.len() - self.limit;
            self.undo.drain(..excess);
        }
    }

    fn discard_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}
