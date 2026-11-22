//! The audio backend abstraction: devices, stream requests, stream handles and health.
//!
//! Every way of reaching audio hardware (cpal hosts, WASAPI exclusive, ASIO through cpal) and
//! the test double ([`crate::mock`]) implements [`AudioBackend`]. A backend opens a
//! [`AudioStream`] whose callback is created on the stream's own thread by a
//! [`CallbackFactory`], so the engine can size its core for the opened device's real format.

use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// `StreamRequest` device id meaning "the system default device".
pub const DEFAULT_DEVICE: &str = "default";

/// Why an I/O operation failed, coarse enough for recovery decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoErrorKind {
    /// The device or host is gone (unplugged, disabled, host not running).
    DeviceLost,
    /// The device is in use by someone else (WASAPI exclusive, JACK client limits, ...).
    Busy,
    /// The requested format, rate or buffer size is not supported.
    Unsupported,
    /// The requested device or port does not exist.
    NotFound,
    /// Anything else.
    Other,
}

/// Failure to open or run an audio or MIDI device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoError {
    kind: IoErrorKind,
    message: String,
}

impl IoError {
    pub fn new(context: &str, cause: impl fmt::Display) -> Self {
        Self::with_kind(IoErrorKind::Other, context, cause)
    }

    pub fn with_kind(kind: IoErrorKind, context: &str, cause: impl fmt::Display) -> Self {
        Self {
            kind,
            message: format!("{context}: {cause}"),
        }
    }

    /// Coarse category, used to decide whether a retry on the default device makes sense.
    pub fn kind(&self) -> IoErrorKind {
        self.kind
    }
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for IoError {}

/// A device a backend can open, as shown in the preferences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable identifier to store in preferences (backend specific).
    pub id: String,
    /// Display name.
    pub name: String,
    pub is_default_output: bool,
    pub is_default_input: bool,
    /// Largest output channel count (0 when the device has no outputs).
    pub max_output_channels: u16,
    /// Largest input channel count (0 when the device has no inputs).
    pub max_input_channels: u16,
    /// Standard sample rates the device reports for output.
    pub sample_rates: Vec<u32>,
    /// Smallest and largest buffer size in frames, when the host reports them.
    pub buffer_frames: Option<(u32, u32)>,
}

/// What to open. `None` fields mean "device default".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamRequest {
    /// Output device id; `None` (or [`DEFAULT_DEVICE`]) is the system default.
    pub output_device: Option<String>,
    /// Input device id; `None` opens no input, [`DEFAULT_DEVICE`] the default input.
    pub input_device: Option<String>,
    pub sample_rate: Option<u32>,
    /// Requested buffer size in frames (a hint on shared hosts, exact on exclusive ones).
    pub buffer_frames: Option<u32>,
    /// Requested output channels; `None` uses the device's default.
    pub output_channels: Option<u16>,
}

/// What was actually opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    /// Backend id (see [`AudioBackend::id`]).
    pub backend: String,
    pub device_id: String,
    pub device_name: String,
    pub sample_rate: u32,
    pub output_channels: u16,
    /// 0 when no input is open.
    pub input_channels: u16,
    /// Callback size in frames when the host reports one.
    pub buffer_frames: Option<u32>,
    /// True for WASAPI exclusive (the device is not shared with other applications).
    pub exclusive: bool,
}

/// The audio callback: fill the interleaved `f32` output; `input` holds the interleaved input
/// for the same frames when an input is open. Called on the realtime thread; it must not
/// allocate, lock or block.
pub type AudioCallback = Box<dyn FnMut(&mut [f32], Option<&[f32]>) + Send>;

/// Creates the callback on the stream's thread once the device format is known.
pub type CallbackFactory = Box<dyn FnOnce(&StreamInfo) -> AudioCallback + Send>;

/// A source of audio devices and streams.
pub trait AudioBackend: Send + Sync {
    /// Stable id stored in preferences, for example `cpal:wasapi`, `wasapi-exclusive`, `mock`.
    fn id(&self) -> String;

    /// Human-readable name for the host selector.
    fn label(&self) -> String;

    /// Lists devices. May be slow; call it from the control thread.
    fn devices(&self) -> Result<Vec<DeviceInfo>, IoError>;

    /// Opens a stream in the stopped state; call [`AudioStream::start`] to run the callback.
    fn open(
        &self,
        request: &StreamRequest,
        factory: CallbackFactory,
    ) -> Result<Box<dyn AudioStream>, IoError>;
}

/// A device stream. Dropping it closes the device.
pub trait AudioStream: Send {
    fn info(&self) -> &StreamInfo;

    /// Starts the callback.
    fn start(&mut self) -> Result<(), IoError>;

    /// Pauses the callback (the device stays open).
    fn stop(&mut self) -> Result<(), IoError>;

    /// Whether the stream is still healthy; polled by the supervisor.
    fn health(&self) -> StreamHealth;
}

/// Health of an open stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamHealth {
    Stopped,
    Running,
    /// The stream died. `device_lost` is true when the device went away.
    Failed {
        reason: String,
        device_lost: bool,
    },
}

const PHASE_STOPPED: u8 = 0;
const PHASE_RUNNING: u8 = 1;
const PHASE_FAILED: u8 = 2;

/// Shared stream state that error callbacks write and the supervisor reads. Writing it never
/// blocks the audio callback (only error paths lock).
#[derive(Debug, Default)]
pub struct StreamState {
    phase: AtomicU8,
    device_lost: AtomicBool,
    reason: Mutex<String>,
}

impl StreamState {
    pub fn set_running(&self, running: bool) {
        if self.phase.load(Ordering::Acquire) == PHASE_FAILED {
            return;
        }
        let phase = if running {
            PHASE_RUNNING
        } else {
            PHASE_STOPPED
        };
        self.phase.store(phase, Ordering::Release);
    }

    /// Marks the stream failed; the first failure wins.
    pub fn fail(&self, device_lost: bool, reason: impl Into<String>) {
        if let Ok(mut slot) = self.reason.lock() {
            if self.phase.load(Ordering::Acquire) == PHASE_FAILED {
                return;
            }
            *slot = reason.into();
        }
        self.device_lost.store(device_lost, Ordering::Release);
        self.phase.store(PHASE_FAILED, Ordering::Release);
    }

    pub fn health(&self) -> StreamHealth {
        match self.phase.load(Ordering::Acquire) {
            PHASE_RUNNING => StreamHealth::Running,
            PHASE_FAILED => StreamHealth::Failed {
                reason: self
                    .reason
                    .lock()
                    .map(|reason| reason.clone())
                    .unwrap_or_default(),
                device_lost: self.device_lost.load(Ordering::Acquire),
            },
            _ => StreamHealth::Stopped,
        }
    }
}

/// Sample rates offered in the preferences when a device reports ranges.
pub const STANDARD_RATES: [u32; 9] = [
    22_050, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800, 384_000,
];

/// Smallest and largest callback size the engine accepts, in frames.
pub const MIN_BUFFER_FRAMES: u32 = 16;
pub const MAX_BUFFER_FRAMES: u32 = 4096;
