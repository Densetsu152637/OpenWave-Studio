//! Typed identifiers for model entities.
//!
//! Every entity (track, bus, clip, pattern, media item, device, automation lane, marker) has a
//! `u64` id drawn from one counter per project ([`Project::alloc_id`](crate::Project::alloc_id)),
//! so a numeric id is unique across all kinds and is never reused, not even after a delete.
//! The engine compiler, the IPC layer and undo entries all rely on that.

use serde::{Deserialize, Serialize};

macro_rules! id_types {
    ($($(#[$meta:meta])* $name:ident),* $(,)?) => {$(
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(type = "number"))]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }

        impl $name {
            /// The raw numeric id.
            pub fn get(self) -> u64 {
                self.0
            }
        }
    )*};
}

id_types! {
    /// Identifies a track.
    TrackId,
    /// Identifies a bus or group.
    BusId,
    /// Identifies a clip (audio, MIDI or pattern instance).
    ClipId,
    /// Identifies a pattern.
    PatternId,
    /// Identifies a media pool item.
    MediaId,
    /// Identifies a device (instrument, effect or hosted plugin).
    DeviceId,
    /// Identifies an automation lane.
    LaneId,
    /// Identifies a marker.
    MarkerId,
}
