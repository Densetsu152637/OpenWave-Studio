//! The plugin main thread (architecture section 4).
//!
//! Plugin formats require some calls on "the main thread": CLAP main-thread calls (instantiating,
//! activating, parameters, state, extensions), VST3 controller and editor calls, native editor
//! windows. This crate never decides which thread that is: everything goes through a
//! [`MainThreadExecutor`].
//!
//! * In the app on Windows and macOS the executor is Tauri's event-loop thread
//!   (`AppHandle::run_on_main_thread`), implemented in the app crate.
//! * On Linux, and in tests, it is a [`ThreadExecutor`]: one dedicated thread that runs the jobs
//!   it is sent. (The next slice makes that thread the editor run loop on Linux.)
//! * [`InlineExecutor`] treats the thread that created it as the main thread and runs jobs on it
//!   directly; the scanner child process uses it.
//!
//! An executor's answer to "is this the main thread?" is what plugins see through CLAP's
//! `thread-check` extension and nothing else.

use std::fmt;
use std::sync::mpsc;
use std::thread::{self, JoinHandle, ThreadId};
use std::time::Duration;

/// A unit of work for the main thread.
pub type Job = Box<dyn FnOnce() + Send + 'static>;

/// How long [`call`] waits for the main thread before giving up (a plugin that hangs the main
/// thread must not hang the caller for ever).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a call to the main thread did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutorError {
    /// The main thread is gone (the event loop ended or the executor was shut down).
    Gone,
    /// The main thread did not answer in [`CALL_TIMEOUT`].
    TimedOut,
}

impl fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gone => f.write_str("the plugin main thread is not running"),
            Self::TimedOut => f.write_str("the plugin main thread did not answer in time"),
        }
    }
}

impl std::error::Error for ExecutorError {}

/// Runs jobs on the plugin main thread.
pub trait MainThreadExecutor: Send + Sync + 'static {
    /// Queues `job` to run on the main thread. Never blocks. Fails when that thread is gone.
    fn dispatch(&self, job: Job) -> Result<(), ExecutorError>;

    /// True when the calling thread is the main thread.
    fn is_main_thread(&self) -> bool;
}

/// Runs `work` on the main thread and returns its result. On the main thread itself the work
/// runs immediately; elsewhere the caller blocks until it is done (or [`CALL_TIMEOUT`] passes).
///
/// Never call this while holding a lock the main thread may also want.
pub fn call<T: Send + 'static>(
    executor: &dyn MainThreadExecutor,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ExecutorError> {
    if executor.is_main_thread() {
        return Ok(work());
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    executor.dispatch(Box::new(move || {
        // The caller may have given up; the answer is then dropped.
        let _ = sender.send(work());
    }))?;
    receiver
        .recv_timeout(CALL_TIMEOUT)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => ExecutorError::TimedOut,
            mpsc::RecvTimeoutError::Disconnected => ExecutorError::Gone,
        })
}

/// A dedicated thread that runs the jobs it is sent, in order.
pub struct ThreadExecutor {
    sender: mpsc::Sender<Job>,
    thread: ThreadId,
    handle: Option<JoinHandle<()>>,
}

impl ThreadExecutor {
    /// Starts the thread.
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        let handle = thread::Builder::new()
            .name("plugin-main".into())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    job();
                }
            })
            .expect("spawn the plugin main thread");
        Self {
            sender,
            thread: handle.thread().id(),
            handle: Some(handle),
        }
    }
}

impl Default for ThreadExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl MainThreadExecutor for ThreadExecutor {
    fn dispatch(&self, job: Job) -> Result<(), ExecutorError> {
        self.sender.send(job).map_err(|_| ExecutorError::Gone)
    }

    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.thread
    }
}

impl Drop for ThreadExecutor {
    fn drop(&mut self) {
        // Closing the channel ends the loop once the queued jobs are done.
        let (closed, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.sender, closed));
        if let Some(handle) = self.handle.take()
            && handle.thread().id() != thread::current().id()
        {
            let _ = handle.join();
        }
    }
}

/// The thread that created it is the main thread; jobs run on the calling thread at once.
pub struct InlineExecutor {
    thread: ThreadId,
}

impl InlineExecutor {
    /// An executor whose main thread is the current thread.
    pub fn new() -> Self {
        Self {
            thread: thread::current().id(),
        }
    }
}

impl Default for InlineExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl MainThreadExecutor for InlineExecutor {
    fn dispatch(&self, job: Job) -> Result<(), ExecutorError> {
        job();
        Ok(())
    }

    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.thread
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_runs_on_the_dedicated_thread_and_returns_its_result() {
        let executor = ThreadExecutor::new();
        let here = thread::current().id();
        let there = call(&executor, || thread::current().id()).unwrap();
        assert_ne!(here, there);
        assert!(!executor.is_main_thread());
        // Nested calls from the main thread run inline instead of deadlocking.
        let nested = call(&executor, {
            let executor = std::sync::Arc::new(ThreadExecutor::new());
            move || call(executor.as_ref(), || 7).unwrap()
        })
        .unwrap();
        assert_eq!(nested, 7);
    }

    #[test]
    fn a_call_from_the_main_thread_runs_immediately() {
        let executor = std::sync::Arc::new(ThreadExecutor::new());
        let inner = std::sync::Arc::clone(&executor);
        let answer = call(executor.as_ref(), move || {
            assert!(inner.is_main_thread());
            call(inner.as_ref(), || 42).unwrap()
        })
        .unwrap();
        assert_eq!(answer, 42);
    }
}
