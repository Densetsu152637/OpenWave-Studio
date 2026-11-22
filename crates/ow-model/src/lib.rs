//! Project model for OpenWave Studio.
//!
//! This crate never depends on `ow-engine`: the engine depends on the model (the compiler from
//! model to render plan lives in `ow-engine`).
//!
//! * [`time`]: beats, the tempo map with linear ramps, the time-signature map and the
//!   beat/sample conversions.
//! * [`model`]: the [`Project`] and its entities (tracks, buses, clips, patterns, media pool,
//!   devices, automation, markers), with typed [`ids`] and the invariants in [`validate`].
//! * [`command`] and [`history`]: every change is an [`EditCommand`] that returns its inverse; the
//!   [`Editor`] keeps undo and redo and groups gestures into transactions.
//! * [`format`](mod@format), [`migrate`], [`bundle`] and [`owz`]: the versioned `project.json`, forward
//!   migration, the `.owave` bundle with atomic save and rotating autosave, and `.owz` archives.
//!
//! Later stages extend the model with additive `#[serde(default)]` fields (no version bump) or
//! bump [`FORMAT_VERSION`] with a migration step and a golden file when a change is not additive.

pub mod bundle;
pub mod command;
pub mod format;
pub mod history;
pub mod ids;
mod keyed;
pub mod migrate;
pub mod model;
pub mod owz;
pub mod time;
mod time_serde;
pub mod validate;

pub use bundle::{BundleError, PROJECT_FILE};
pub use command::{DeviceBundle, DeviceSlot, EditCommand, EditError, Owned};
pub use format::{FORMAT_VERSION, FormatError, from_json, to_json};
pub use history::Editor;
pub use ids::{BusId, ClipId, DeviceId, LaneId, MarkerId, MediaId, PatternId, TrackId};
pub use model::*;
pub use validate::ValidationError;
