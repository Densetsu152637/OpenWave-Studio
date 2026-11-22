//! Audio and MIDI device layer for OpenWave Studio.
//!
//! - [`AudioBackend`] / [`AudioStream`]: device enumeration, open, start, stop, format and health
//!   for every audio API. Implementations: [`CpalBackend`] (WASAPI shared, CoreAudio, ALSA, and
//!   JACK, PipeWire or ASIO when built in), [`WasapiExclusiveBackend`] (Windows exclusive mode)
//!   and [`mock::MockBackend`] (hardware-free realtime path for tests).
//! - [`Supervisor`]: keeps a stream alive, reopening after device loss with bounded retries and
//!   switching device, rate or buffer size on request.
//! - [`MidiBackend`]: MIDI ports through `midir`, with [`mock::MockMidiBackend`] for tests. Each
//!   input connection is a separate producer for its own ring to the audio thread.
//!
//! The audio callback never allocates: conversion scratch is allocated before a stream starts.
//! ASIO is source-build-only: the `asio` feature is off by default and official binaries never
//! enable it (see `docs/audio-devices.md`).

pub mod backend;
mod cpal_backend;
pub mod midi;
pub mod mock;
pub mod supervisor;
mod threaded;
#[cfg(windows)]
mod wasapi_exclusive;

use std::sync::Arc;

pub use backend::{
    AudioBackend, AudioCallback, AudioStream, CallbackFactory, DEFAULT_DEVICE, DeviceInfo, IoError,
    IoErrorKind, MAX_BUFFER_FRAMES, MIN_BUFFER_FRAMES, STANDARD_RATES, StreamHealth, StreamInfo,
    StreamRequest, StreamState,
};
pub use cpal_backend::CpalBackend;
pub use midi::{
    MidiBackend, MidiEvent, MidiHandler, MidiInputConnection, MidiOutputConnection, MidiPortInfo,
    MidirBackend, now_micros,
};
pub use supervisor::{AudioSelection, AudioState, AudioStatus, Opener, RecoveryPolicy, Supervisor};
#[cfg(windows)]
pub use wasapi_exclusive::{WASAPI_EXCLUSIVE_ID, WasapiExclusiveBackend};

/// The audio backends available on this platform, in preference order: the default cpal host
/// first, then the other hosts, then exclusive mode where it exists.
#[derive(Clone)]
pub struct BackendRegistry {
    backends: Vec<Arc<dyn AudioBackend>>,
}

impl BackendRegistry {
    /// Every real backend for this platform and build.
    pub fn system() -> Self {
        let mut backends: Vec<Arc<dyn AudioBackend>> = CpalBackend::available()
            .into_iter()
            .map(|backend| Arc::new(backend) as Arc<dyn AudioBackend>)
            .collect();
        #[cfg(windows)]
        backends.push(Arc::new(WasapiExclusiveBackend));
        Self { backends }
    }

    /// A registry with exactly these backends (tests use it with the mock backend).
    pub fn with(backends: Vec<Arc<dyn AudioBackend>>) -> Self {
        Self { backends }
    }

    pub fn all(&self) -> &[Arc<dyn AudioBackend>] {
        &self.backends
    }

    /// The backend with this id; an empty or unknown id selects the first (default) one.
    pub fn resolve(&self, id: &str) -> Option<&Arc<dyn AudioBackend>> {
        self.backends
            .iter()
            .find(|backend| backend.id() == id)
            .or_else(|| self.backends.first())
    }

    /// Opens a stream for `selection`; see [`resolve`](Self::resolve) for backend choice.
    pub fn open(
        &self,
        selection: &AudioSelection,
        factory: CallbackFactory,
    ) -> Result<Box<dyn AudioStream>, IoError> {
        let backend = self
            .resolve(&selection.backend)
            .ok_or_else(|| IoError::new("opening audio", "no audio backend is available"))?;
        backend.open(&selection.request, factory)
    }
}
