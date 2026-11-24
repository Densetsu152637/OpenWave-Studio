//! Effects: devices that process the audio of the track they are inserted on.
//!
//! An effect reads its input from the buffers it is given, overwrites them with its output and
//! ignores note events.

pub mod compressor;
pub mod eq;
pub mod limiter;
pub mod reverb;
