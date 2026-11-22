//! Device-loss recovery and live reconfiguration of the running audio stream.
//!
//! A [`Supervisor`] owns the current [`AudioStream`] and a background thread that polls its
//! health. When the stream fails (device unplugged, endpoint invalidated, host restarted) the
//! thread drops it and reopens the chosen device with bounded retries and growing delays; after a
//! few failures it falls back to the system default device on the same backend. If every attempt
//! fails the state becomes [`AudioState::Failed`] with the last error, which the UI shows; the
//! user can pick another device or call [`Supervisor::retry`].
//!
//! Opening is delegated to an [`Opener`] so the owner (the engine) can build a fresh audio core
//! for each stream: a new device may have a different sample rate or channel count.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::backend::{AudioStream, IoError, StreamHealth, StreamInfo, StreamRequest};

/// A backend choice plus what to open on it. Stored in preferences.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioSelection {
    /// Backend id (`cpal:wasapi`, `wasapi-exclusive`, `mock`, ...), or empty for the default
    /// host.
    pub backend: String,
    pub request: StreamRequest,
}

/// Opens (but does not start) a stream for a selection.
pub type Opener = Box<dyn FnMut(&AudioSelection) -> Result<Box<dyn AudioStream>, IoError> + Send>;

/// Retry behaviour after a failure.
#[derive(Debug, Clone, Copy)]
pub struct RecoveryPolicy {
    /// Reopen attempts per failure before giving up.
    pub max_attempts: u32,
    /// Delay before the first attempt; doubles each attempt up to `max_delay`.
    pub initial_delay: Duration,
    pub max_delay: Duration,
    /// Attempts on the chosen device before falling back to the default device.
    pub attempts_before_fallback: u32,
    /// How often the stream's health is checked.
    pub poll_interval: Duration,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(4),
            attempts_before_fallback: 2,
            poll_interval: Duration::from_millis(100),
        }
    }
}

impl RecoveryPolicy {
    /// Delay before 1-based `attempt`.
    pub fn delay_for(&self, attempt: u32) -> Duration {
        let factor = 1_u32
            .checked_shl(attempt.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.initial_delay
            .checked_mul(factor)
            .unwrap_or(self.max_delay)
            .min(self.max_delay)
    }
}

/// Where the supervisor stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioState {
    /// A stream is open and running.
    Running,
    /// The stream failed; reopening (1-based attempt out of `max`).
    Recovering { attempt: u32, max: u32 },
    /// No stream: every attempt failed or none was possible. See `error`.
    Failed,
}

/// Snapshot for the UI and status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioStatus {
    pub state: AudioState,
    /// The open stream, when running.
    pub info: Option<StreamInfo>,
    /// Last failure, kept while recovering and after giving up; cleared once running again.
    pub error: Option<String>,
    /// Successful reopenings after failures since start.
    pub recoveries: u32,
    /// The selection the user asked for.
    pub selection: AudioSelection,
    /// True while running on the default device because the chosen one could not be opened.
    pub on_fallback_device: bool,
}

struct Inner {
    opener: Opener,
    stream: Option<Box<dyn AudioStream>>,
    selection: AudioSelection,
}

struct Shared {
    inner: Mutex<Inner>,
    status: Mutex<AudioStatus>,
    /// Bumped by reconfigure and retry so a recovery in progress notices and stops.
    generation: AtomicU64,
    /// Set when no stream is open and recovery should run (initial failure, `retry`).
    needs_recovery: AtomicBool,
    stop: AtomicBool,
    policy: RecoveryPolicy,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Owns the stream and keeps it alive. Dropping it closes the stream and ends the thread.
pub struct Supervisor {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Supervisor {
    /// Opens and starts `selection`. A failure is not fatal: the status becomes
    /// [`AudioState::Recovering`] and the thread keeps trying under `policy`.
    pub fn start(mut opener: Opener, selection: AudioSelection, policy: RecoveryPolicy) -> Self {
        let first = open_and_start(&mut opener, &selection);
        let (stream, error) = match first {
            Ok(stream) => (Some(stream), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let status = AudioStatus {
            state: if stream.is_some() {
                AudioState::Running
            } else {
                AudioState::Failed
            },
            info: stream.as_ref().map(|s| s.info().clone()),
            error,
            recoveries: 0,
            selection: selection.clone(),
            on_fallback_device: false,
        };
        let needs_recovery = stream.is_none();
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                opener,
                stream,
                selection,
            }),
            status: Mutex::new(status),
            generation: AtomicU64::new(0),
            needs_recovery: AtomicBool::new(needs_recovery),
            stop: AtomicBool::new(false),
            policy,
        });
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("ow-audio-supervisor".into())
            .spawn(move || supervise(&thread_shared))
            .ok();
        Self { shared, thread }
    }

    pub fn status(&self) -> AudioStatus {
        lock(&self.shared.status).clone()
    }

    /// Switches to `selection` (device, rate, buffer size or backend). The old stream is closed
    /// first, since exclusive devices cannot be opened twice. On failure the previous selection
    /// is reopened if possible and the error is returned.
    pub fn reconfigure(&self, selection: AudioSelection) -> Result<AudioStatus, IoError> {
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        let mut inner = lock(&self.shared.inner);
        let previous = inner.selection.clone();
        inner.stream = None;
        let result = open_and_start(&mut inner.opener, &selection);
        match result {
            Ok(stream) => {
                inner.selection = selection.clone();
                publish_running(&self.shared, stream.as_ref(), selection, false);
                inner.stream = Some(stream);
                drop(inner);
                self.shared.needs_recovery.store(false, Ordering::Release);
                Ok(self.status())
            }
            Err(error) => {
                // Best effort: go back to what worked.
                let restored = open_and_start(&mut inner.opener, &previous);
                match restored {
                    Ok(stream) => {
                        publish_running(&self.shared, stream.as_ref(), previous, false);
                        inner.stream = Some(stream);
                        lock(&self.shared.status).error = Some(error.to_string());
                    }
                    Err(_) => {
                        let mut status = lock(&self.shared.status);
                        status.state = AudioState::Failed;
                        status.info = None;
                        status.error = Some(error.to_string());
                        status.selection = selection;
                        drop(status);
                        self.shared.needs_recovery.store(true, Ordering::Release);
                    }
                }
                Err(error)
            }
        }
    }

    /// Starts a fresh round of reopen attempts after the supervisor gave up.
    pub fn retry(&self) {
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        if lock(&self.shared.inner).stream.is_none() {
            self.shared.needs_recovery.store(true, Ordering::Release);
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        lock(&self.shared.inner).stream = None;
    }
}

fn publish_running(
    shared: &Shared,
    stream: &dyn AudioStream,
    selection: AudioSelection,
    fallback: bool,
) {
    let mut status = lock(&shared.status);
    status.state = AudioState::Running;
    status.info = Some(stream.info().clone());
    status.error = None;
    status.selection = selection;
    status.on_fallback_device = fallback;
}

fn open_and_start(
    opener: &mut Opener,
    selection: &AudioSelection,
) -> Result<Box<dyn AudioStream>, IoError> {
    let mut stream = opener(selection)?;
    stream.start()?;
    Ok(stream)
}

/// The selection with the device choice reset to the system default (keeps backend and format
/// hints that the default device may still honour). The input device is dropped entirely so an
/// output-only open can succeed when the machine has no default input.
fn fallback_selection(selection: &AudioSelection) -> AudioSelection {
    AudioSelection {
        backend: selection.backend.clone(),
        request: StreamRequest {
            output_device: None,
            sample_rate: None,
            buffer_frames: selection.request.buffer_frames,
            output_channels: None,
            input_device: None,
        },
    }
}

/// Sleeps up to `total`, returning early (false) when the supervisor is stopping or the
/// generation changed.
fn interruptible_sleep(shared: &Shared, total: Duration, generation: u64) -> bool {
    let end = Instant::now() + total;
    while Instant::now() < end {
        if shared.stop.load(Ordering::Acquire)
            || shared.generation.load(Ordering::Acquire) != generation
        {
            return false;
        }
        thread::sleep(Duration::from_millis(20).min(total));
    }
    true
}

fn supervise(shared: &Shared) {
    while !shared.stop.load(Ordering::Acquire) {
        thread::sleep(shared.policy.poll_interval);
        let failure = {
            let mut inner = lock(&shared.inner);
            match inner.stream.as_ref().map(|stream| stream.health()) {
                Some(StreamHealth::Failed { reason, .. }) => {
                    // Release the device before reopening it.
                    inner.stream = None;
                    Some(reason)
                }
                _ => None,
            }
        };
        let recover_reason = match failure {
            Some(reason) => Some(reason),
            None if shared.needs_recovery.swap(false, Ordering::AcqRel) => {
                lock(&shared.status).error.clone()
            }
            None => None,
        };
        if let Some(reason) = recover_reason {
            recover(shared, reason);
        }
    }
}

/// Republishes `Running` from the live stream when a reconfigure or retry installed one while
/// recovery was in progress and the status still says otherwise.
fn republish_if_stale(shared: &Shared, inner: &Inner) {
    let Some(stream) = inner.stream.as_ref() else {
        return;
    };
    let stale = {
        let status = lock(&shared.status);
        status.state != AudioState::Running || status.info.is_none()
    };
    if stale {
        publish_running(shared, stream.as_ref(), inner.selection.clone(), false);
    }
}

/// Publishes `Recovering` under the `inner` lock, so it cannot overwrite the status of a stream
/// that a concurrent reconfigure or retry installed. Returns the locked state when recovery
/// should continue, `None` when it should stop.
fn publish_recovering<'a>(
    shared: &'a Shared,
    generation: u64,
    attempt: u32,
    reason: &str,
) -> Option<MutexGuard<'a, Inner>> {
    let inner = lock(&shared.inner);
    if inner.stream.is_some() {
        republish_if_stale(shared, &inner);
        return None;
    }
    if shared.generation.load(Ordering::Acquire) != generation
        || shared.stop.load(Ordering::Acquire)
    {
        return None;
    }
    let mut status = lock(&shared.status);
    status.state = AudioState::Recovering {
        attempt,
        max: shared.policy.max_attempts,
    };
    status.info = None;
    status.error = Some(reason.to_string());
    drop(status);
    Some(inner)
}

fn recover(shared: &Shared, reason: String) {
    let generation = shared.generation.load(Ordering::Acquire);
    let policy = shared.policy;
    if publish_recovering(shared, generation, 0, &reason).is_none() {
        return;
    }
    let mut last_error = reason;
    for attempt in 1..=policy.max_attempts {
        if publish_recovering(shared, generation, attempt, &last_error).is_none() {
            return;
        }
        if !interruptible_sleep(shared, policy.delay_for(attempt), generation) {
            return;
        }
        let mut inner = lock(&shared.inner);
        if inner.stream.is_some() {
            republish_if_stale(shared, &inner);
            return;
        }
        if shared.generation.load(Ordering::Acquire) != generation {
            return;
        }
        let fallback = attempt > policy.attempts_before_fallback;
        let selection = if fallback {
            fallback_selection(&inner.selection)
        } else {
            inner.selection.clone()
        };
        match open_and_start(&mut inner.opener, &selection) {
            Ok(stream) => {
                let mut status = lock(&shared.status);
                status.state = AudioState::Running;
                status.info = Some(stream.info().clone());
                status.error = None;
                status.recoveries += 1;
                status.on_fallback_device = fallback;
                drop(status);
                inner.stream = Some(stream);
                return;
            }
            Err(error) => last_error = error.to_string(),
        }
    }
    let inner = lock(&shared.inner);
    if inner.stream.is_some() {
        republish_if_stale(shared, &inner);
        return;
    }
    if shared.generation.load(Ordering::Acquire) != generation {
        return;
    }
    let mut status = lock(&shared.status);
    status.state = AudioState::Failed;
    status.info = None;
    status.error = Some(last_error);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_double_up_to_the_cap() {
        let policy = RecoveryPolicy {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
            ..RecoveryPolicy::default()
        };
        let delays: Vec<u128> = (1..=5).map(|a| policy.delay_for(a).as_millis()).collect();
        assert_eq!(delays, [100, 200, 400, 500, 500]);
        assert_eq!(policy.delay_for(200), Duration::from_millis(500));
    }

    #[test]
    fn fallback_resets_devices_and_drops_the_input_but_keeps_the_buffer_hint() {
        let selection = AudioSelection {
            backend: "cpal:wasapi".into(),
            request: StreamRequest {
                output_device: Some("usb".into()),
                input_device: Some("usb-in".into()),
                sample_rate: Some(96_000),
                buffer_frames: Some(128),
                output_channels: Some(8),
            },
        };
        let fallback = fallback_selection(&selection);
        assert_eq!(fallback.backend, "cpal:wasapi");
        assert_eq!(fallback.request.output_device, None);
        assert_eq!(fallback.request.sample_rate, None);
        assert_eq!(fallback.request.buffer_frames, Some(128));
        assert_eq!(fallback.request.input_device, None);
    }
}
