//! Project model for OpenWave Studio.
//!
//! This crate never depends on `ow-engine`: the engine depends on the model (the compiler from
//! model to render plan lives in `ow-engine`). It currently holds the timeline math
//! ([`time`]): beats, the tempo map with linear ramps, the time-signature map and the
//! beat/sample conversions. Later stages add tracks, clips, edit commands and serialization
//! next to it.

pub mod time;
