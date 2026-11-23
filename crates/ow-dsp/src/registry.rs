//! The built-in device kinds.
//!
//! Effects are added to [`ALL`] next to the synth; the engine, the IPC layer and the UI all go
//! through this list, so a new kind needs no change anywhere else.

use crate::device::DeviceInfo;
use crate::instruments::synth;

/// Every built-in device kind, instruments first.
pub static ALL: &[DeviceInfo] = &[synth::INFO];

/// Every built-in device kind.
pub fn all() -> &'static [DeviceInfo] {
    ALL
}

/// The built-in kind named `kind` (the string stored in `Device::kind`).
pub fn find(kind: &str) -> Option<&'static DeviceInfo> {
    ALL.iter().find(|info| info.kind == kind)
}
