//! The IPC contract between the OpenWave Studio engine and its UI (architecture section 7).
//!
//! This crate depends only on `ow-model` (and `ow-dsp`, for the device descriptors); the Tauri glue lives in `src-tauri/src/ipc/` and the UI
//! side in `src/lib/ipc/` and `src/lib/stores/`.
//!
//! * [`dto`]: the commands the UI sends ([`UiCommand`], wrapping `ow-model`'s `EditCommand`), the
//!   [`Snapshot`] and the ordered [`ModelPatch`] stream. TypeScript types are generated with
//!   `ts-rs` into `src/lib/ipc/generated/` and committed; `cargo test -p ow-ipc export_bindings`
//!   regenerates them and CI fails when they are stale.
//! * [`session`]: the control-thread [`Session`] (the authoritative `Editor` plus the patch
//!   counter). Every applied command, undo and redo yields at most one patch, numbered one higher
//!   than the last.
//! * [`mirror`]: [`diff`] computes a patch's operations; [`Mirror`] is the Rust reference of what
//!   the UI stores do: apply patches strictly in order and resynchronize from a snapshot on a
//!   gap. The UI never mutates its mirror.
//! * [`telemetry`]: the [`TelemetryBank`] of atomics the audio thread writes, and the packed
//!   little-endian `f32` frame the bridge thread sends over a Tauri `ipc::Channel` (layout in
//!   that module's docs).
//! * [`devices`]: the built-in device descriptors ([`device_descriptors`]) and the wire shape of
//!   scanned plugins the UI builds its generic device panels from.
//! * [`analysis`]: the request and report types of offline analysis and of export with
//!   loudness normalisation (the measurements come from `ow-media`).
//! * [`peaks`]: the raw-byte waveform range response (format only; the data comes from the `ow-media` peak cache).
//!
//! # Threads
//!
//! The audio thread touches only the [`TelemetryBank`] (atomic stores). The control thread owns
//! the [`Session`]; the UI bridge thread owns a [`FramePacker`] and reads the bank. Nothing on the
//! audio thread talks to IPC.
//!
//! # Resync protocol
//!
//! 1. The UI attaches: one call registers its patch channel and returns a [`Snapshot`] taken
//!    under the same lock, so no patch can fall between them.
//! 2. It applies patches numbered `snapshot.seq + 1`, `+ 2`, ... A duplicate (at or below its
//!    `seq`) is ignored; anything beyond the next number is a gap.
//! 3. On a gap it discards what it holds, asks for a fresh snapshot and continues from there.

pub mod analysis;
pub mod devices;
pub mod dto;
pub mod mirror;
pub mod peaks;
pub mod session;
pub mod telemetry;

pub use analysis::{
    AnalysisReport, AnalysisRequest, BandEnergy, ExportOptions, ExportResult, NormalizationReport,
    NormalizeOptions,
};
pub use devices::{
    DeviceDescriptor, ParamDescriptorDto, ParamScale, ParamUnit, PluginScanFailure,
    PluginScanSummary, device_descriptors,
};
pub use dto::{
    CommandReply, HistoryState, MAX_ALLOC_IDS, MediaImportReply, MidiEditReply, MidiExportReply,
    MidiImportReply, ModelPatch, PatchOp, Snapshot, UiCommand,
};
pub use mirror::{Gap, Mirror, diff};
pub use session::{Outcome, Session};
pub use telemetry::{
    FRAME_VERSION, Frame, FramePacker, HEADER_WORDS, MASTER_SLOT, METER_SLOTS, Overflow,
    STRIP_WORDS, StripMeter, Subscriptions, TRANSPORT_COUNTING_IN, TRANSPORT_PLAYING,
    TRANSPORT_RECORDING, TelemetryBank, meter_slot,
};

/// The model types the DTOs carry, re-exported so the app crate needs only `ow-ipc` for them.
pub use ow_model::{EditCommand, EditError, Editor, MixerRef, Project};
