//! Transport, timeline and sample-accurate block timing.
//!
//! - The transport (play, stop, record arm, loop, seek, punch range, metronome with count-in) is
//!   state the audio thread owns and changes through [`TransportCommand`]s on the message
//!   rings.
//! - The tempo map and time-signature map arrive as an immutable [`Timeline`] swapped in through
//!   its own ring ([`ControlLink::swap_timeline`](crate::ControlLink::swap_timeline)); retired
//!   timelines return to the control thread to be freed.
//! - Nodes see each block through [`BlockTiming`] ([`ProcessContext::timing`]): which sample
//!   ranges map to which beat ranges, with loop wraps cut into [`Span`]s, so clip and automation
//!   stages can place events on exact frames.
//! - [`MetronomeNode`] turns the clicks the timing reports into audio.
//!
//! Nothing here allocates, locks or blocks on the audio thread.
//!
//! [`ProcessContext::timing`]: crate::ProcessContext::timing

mod block;
mod metronome;
mod state;
mod timeline;

pub(crate) use block::BlockCore;
pub use block::{BlockTiming, Click, Span, Spans};
pub use metronome::{
    ACCENT_HZ, ACCENT_LEVEL, BEAT_HZ, BEAT_LEVEL, METRONOME_PARAM_GAIN, MetronomeNode,
};
pub(crate) use state::TransportState;
pub use state::{Transport, TransportCommand};
pub use timeline::Timeline;

/// A timeline was not queued because the timeline ring is full; it is handed back.
pub struct TimelineSwapError(pub Box<Timeline>);

impl std::fmt::Debug for TimelineSwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TimelineSwapError(timeline ring full)")
    }
}

impl std::fmt::Display for TimelineSwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the timeline ring is full")
    }
}

impl std::error::Error for TimelineSwapError {}
