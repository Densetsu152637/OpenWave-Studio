//! MIDI editing, capture and file conversion: pure functions on notes, shared by the piano roll,
//! the MIDI recorder and the MCP tools so every caller gets the same result.
//!
//! * [`ops`]: [`MidiEdit`], the note operations (quantize, legato, humanize, transpose, velocity)
//!   and the functions behind them. Every operation is deterministic; humanize takes a seed.
//! * [`capture`]: turns recorded MIDI events into notes and controllers, cuts a range out of a
//!   clip's content (record `replace`) and merges notes (record `overdub`).
//! * [`file`]: Standard MIDI Files (`midly`): import a `.mid` into per-track notes and export
//!   tracks back. Works on bytes; reading and writing the file is the caller's job.
//!
//! One beat is one quarter note everywhere in this module, the unit of a file's pulses per
//! quarter note.

pub mod capture;
pub mod file;
pub mod ops;

pub use capture::{MidiPiece, RecordedEvent, carve, merge_notes, notes_from_events};
pub use file::{ExportTrack, ImportedMidi, ImportedTrack, MidiFileError, export, import};
pub use ops::{
    LegatoMode, MidiEdit, humanize, legato, quantize, scale_velocity, set_velocity, transpose,
};

/// Shortest note a capture or an edit produces, in beats (1/960 of a quarter note).
pub const MIN_NOTE_LENGTH: f64 = 1.0 / 960.0;
