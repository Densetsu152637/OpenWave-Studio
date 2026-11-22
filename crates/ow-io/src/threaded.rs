//! A stream whose device objects live on a dedicated thread.
//!
//! Some host APIs (cpal on several platforms, COM-based WASAPI) require the stream to be created
//! and driven on one thread. [`ThreadedStream`] is the `Send` handle: it forwards start and stop
//! commands and joins the thread on drop, which closes the device.

use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

use crate::backend::{AudioStream, IoError, StreamHealth, StreamInfo, StreamState};

/// Reply channel for a command.
pub(crate) type Reply = Sender<Result<(), IoError>>;

/// Command executed by the stream thread.
pub(crate) enum Command {
    Start(Reply),
    Stop(Reply),
}

/// Handle to a stream owned by its thread. Dropping it closes the device.
pub(crate) struct ThreadedStream {
    info: StreamInfo,
    state: Arc<StreamState>,
    commands: Option<Sender<Command>>,
    thread: Option<JoinHandle<()>>,
}

impl ThreadedStream {
    pub(crate) fn new(
        info: StreamInfo,
        state: Arc<StreamState>,
        commands: Sender<Command>,
        thread: JoinHandle<()>,
    ) -> Self {
        Self {
            info,
            state,
            commands: Some(commands),
            thread: Some(thread),
        }
    }

    fn roundtrip(&self, make: impl FnOnce(Reply) -> Command) -> Result<(), IoError> {
        let gone = || IoError::new("stream command", "stream thread exited");
        let (tx, rx) = mpsc::channel();
        self.commands
            .as_ref()
            .ok_or_else(gone)?
            .send(make(tx))
            .map_err(|_| gone())?;
        rx.recv().map_err(|_| gone())?
    }
}

impl AudioStream for ThreadedStream {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn start(&mut self) -> Result<(), IoError> {
        self.roundtrip(Command::Start)
    }

    fn stop(&mut self) -> Result<(), IoError> {
        self.roundtrip(Command::Stop)
    }

    fn health(&self) -> StreamHealth {
        self.state.health()
    }
}

impl Drop for ThreadedStream {
    fn drop(&mut self) {
        // Closing the command channel ends the stream thread, which closes the device.
        self.commands.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
