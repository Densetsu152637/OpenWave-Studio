//! Third-party plugin hosting for OpenWave Studio (architecture section 8).
//!
//! This crate knows plugin formats and nothing about the project model or the engine: it
//! depends only on `ow-dsp` for the note [`Event`](ow_dsp::Event) type, and presents a hosted
//! plugin as an `ow-dsp` [`Device`](ow_dsp::Device) ([`PluginDevice`]), which the engine already
//! knows how to run as a stereo graph node.
//!
//! * [`MainThreadExecutor`]: the one place that decides which thread is "the plugin main
//!   thread" ([`executor`]).
//! * [`HostedPlugin`] / [`PluginProcessor`]: one instance, split into the control-side object
//!   and the audio-thread half ([`hosted`]). [`clap`] implements them on `clack-host`; VST3 will
//!   implement the same traits.
//! * [`PluginCache`]: instances by device id, surviving recompiles ([`cache`]).
//! * [`Scanner`] and `ow-scan`: finding plugin files and reading them in a child process, with a
//!   timeout and a cache keyed by path, size and modification time ([`scan`], [`info`]).
//!
//! `docs/plugins.md` describes the user-facing side.

pub mod cache;
pub mod clap;
pub mod device;
pub mod executor;
pub mod hosted;
pub mod info;
pub mod scan;

pub use cache::{PluginCache, PluginDirectory};
pub use device::PluginDevice;
pub use executor::{ExecutorError, InlineExecutor, MainThreadExecutor, ThreadExecutor};
pub use hosted::{HostedPlugin, PluginError, PluginProcessor, Requests};
pub use info::{
    FileRecord, Format, ParamInfo, PluginInfo, ScanCache, ScanOutput, ScanStatus, is_plugin_kind,
    kind_of, parse_kind,
};
pub use scan::{ScanCommand, ScanReport, Scanner, default_paths};

/// Encodes plugin state bytes as the base64 text `StateChunk::data` holds.
pub fn encode_state(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decodes `StateChunk::data`; `None` when it is not valid base64.
pub fn decode_state(text: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}
