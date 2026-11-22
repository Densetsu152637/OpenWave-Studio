//! The transport state machine the audio thread owns: play, stop, record arm, loop, seek, punch
//! range, metronome and count-in.
//!
//! Everything is integer sample math on a 64-bit position; beats only enter when a command or a
//! timeline swap fixes loop, punch and count-in bounds in samples. A block's outcome therefore
//! depends only on the position and the frame count, never on how the device buffer was split,
//! which is what makes loop playback gap-free and renders independent of the block size.

use super::block::BlockCore;
use super::timeline::Timeline;

/// Transport state handed to nodes each block (the state at the start of the block).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Transport {
    /// Whether the transport is running: advancing and, if looping, wrapping. False while a
    /// count-in is running even though play was requested.
    pub playing: bool,
    /// Position in samples at the start of the block. During a count-in this is where playback
    /// will start.
    pub position: u64,
    /// Whether recording is active: armed and running (and past the count-in). Within a block
    /// the punch range narrows it per frame, see [`Span::record_frames`](super::Span::record_frames).
    pub recording: bool,
    /// Whether a count-in is running at the start of the block.
    pub counting_in: bool,
}

/// A transport change sent through a message ring. Commands apply at the start of the next
/// cycle, in ring order, so a position or range given in beats is converted with the timeline
/// that is live then.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransportCommand {
    /// Start advancing the position. With record armed and a count-in of one or more bars, the
    /// count-in runs first and the position holds until it ends.
    Play,
    /// Stop advancing the position and cancel any count-in.
    Stop,
    /// Jump to a sample position (during playback too; takes effect at the next cycle).
    Locate(u64),
    /// Jump to a position in beats.
    LocateBeats(f64),
    /// Arm or disarm recording.
    SetRecordArm(bool),
    /// Set the loop range in beats and switch looping on or off. A range whose end does not
    /// come after its start (in whole samples) disables the loop.
    SetLoop {
        /// Whether looping is on.
        enabled: bool,
        /// Loop start, in beats.
        start_beats: f64,
        /// Loop end (exclusive), in beats.
        end_beats: f64,
    },
    /// Set the punch range in beats and switch it on or off. While on, only frames inside the
    /// range count as recording. An empty range disables it.
    SetPunch {
        /// Whether the punch range is on.
        enabled: bool,
        /// Punch-in, in beats.
        in_beats: f64,
        /// Punch-out (exclusive), in beats.
        out_beats: f64,
    },
    /// Switch the metronome on or off and set the count-in length in bars (0 for none). The
    /// count-in plays, with clicks, only when recording is armed.
    SetMetronome {
        /// Whether the metronome clicks during playback.
        enabled: bool,
        /// Bars of count-in before recording starts.
        count_in_bars: u32,
    },
}

/// A running count-in: `beats` clicks `unit` quarter-note beats apart starting at `start_beat`,
/// `total` samples long in all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CountIn {
    pub(crate) start_beat: f64,
    /// Length of one click interval in quarter-note beats.
    pub(crate) unit: f64,
    /// Total clicks (bars times beats per bar).
    pub(crate) beats: u32,
    /// Clicks per bar; every `per_bar`-th click is accented.
    pub(crate) per_bar: u32,
    /// Fractional sample position of `start_beat`.
    pub(crate) base: f64,
    /// Length of the count-in in samples.
    pub(crate) total: u64,
    /// Samples played so far.
    pub(crate) elapsed: u64,
    /// Index of the first click not yet played.
    pub(crate) next_click: u32,
}

impl CountIn {
    fn start(timeline: &Timeline, sample_rate: u32, position: u64, bars: u32) -> Self {
        let start_beat = timeline.sample_to_beat(position, sample_rate);
        let signature = timeline.signatures().signature_at_beat(start_beat);
        let unit = signature.beat_length();
        let beats = bars.saturating_mul(signature.numerator);
        let base = timeline.beat_to_sample(start_beat, sample_rate);
        let end = timeline.beat_to_sample(start_beat + f64::from(beats) * unit, sample_rate);
        Self {
            start_beat,
            unit,
            beats,
            per_bar: signature.numerator,
            base,
            total: ow_model::time::sample_index(end - base).max(1),
            elapsed: 0,
            next_click: 0,
        }
    }

    /// Sample offset from the start of the count-in at which click `index` sounds.
    pub(crate) fn click_offset(&self, index: u32, timeline: &Timeline, sample_rate: u32) -> u64 {
        let at =
            timeline.beat_to_sample(self.start_beat + f64::from(index) * self.unit, sample_rate);
        ow_model::time::sample_index(at - self.base)
    }
}

/// The position after `frames` more frames of playback from `position`, wrapping at the loop end.
///
/// With a loop `[start, end)` and `position < end`, playback runs to `end` and continues from
/// `start`, repeating as often as `frames` requires (a loop shorter than the block wraps several
/// times). From a position at or past the loop end playback just continues.
pub(crate) fn advance_position(position: u64, frames: u64, looped: Option<(u64, u64)>) -> u64 {
    match looped {
        Some((start, end)) if position < end => {
            let target = position + frames;
            if target < end {
                target
            } else {
                start + (target - end) % (end - start)
            }
        }
        _ => position + frames,
    }
}

/// Transport state on the audio thread.
#[derive(Debug, Clone)]
pub(crate) struct TransportState {
    running: bool,
    armed: bool,
    position: u64,
    loop_beats: Option<(f64, f64)>,
    punch_beats: Option<(f64, f64)>,
    loop_samples: Option<(u64, u64)>,
    punch_samples: Option<(u64, u64)>,
    metronome: bool,
    count_in_bars: u32,
    count_in: Option<CountIn>,
    sample_rate: u32,
}

impl TransportState {
    pub(crate) fn new() -> Self {
        Self {
            running: false,
            armed: false,
            position: 0,
            loop_beats: None,
            punch_beats: None,
            loop_samples: None,
            punch_samples: None,
            metronome: false,
            count_in_bars: 0,
            count_in: None,
            sample_rate: 48_000,
        }
    }

    /// The node-facing state at the current position.
    pub(crate) fn summary(&self) -> Transport {
        let live = self.running && self.count_in.is_none();
        Transport {
            playing: live,
            position: self.position,
            recording: live && self.armed,
            counting_in: self.count_in.is_some(),
        }
    }

    pub(crate) fn position(&self) -> u64 {
        self.position
    }

    /// Applies a command from a ring.
    pub(crate) fn apply(&mut self, command: TransportCommand, timeline: &Timeline) {
        match command {
            TransportCommand::Play => {
                if !self.running {
                    self.running = true;
                    if self.armed && self.count_in_bars > 0 {
                        self.count_in = Some(CountIn::start(
                            timeline,
                            self.sample_rate,
                            self.position,
                            self.count_in_bars,
                        ));
                    }
                }
            }
            TransportCommand::Stop => {
                self.running = false;
                self.count_in = None;
            }
            TransportCommand::Locate(position) => self.position = position,
            TransportCommand::LocateBeats(beats) => {
                self.position = timeline.beat_to_sample_index(beats, self.sample_rate);
            }
            TransportCommand::SetRecordArm(armed) => {
                self.armed = armed;
                if !armed {
                    self.count_in = None;
                }
            }
            TransportCommand::SetLoop {
                enabled,
                start_beats,
                end_beats,
            } => {
                self.loop_beats = enabled.then_some((start_beats, end_beats));
                self.refresh_bounds(timeline);
            }
            TransportCommand::SetPunch {
                enabled,
                in_beats,
                out_beats,
            } => {
                self.punch_beats = enabled.then_some((in_beats, out_beats));
                self.refresh_bounds(timeline);
            }
            TransportCommand::SetMetronome {
                enabled,
                count_in_bars,
            } => {
                self.metronome = enabled;
                self.count_in_bars = count_in_bars;
            }
        }
    }

    /// Re-derives the loop and punch bounds in samples from their beats.
    fn refresh_bounds(&mut self, timeline: &Timeline) {
        let rate = self.sample_rate;
        let bounds = |beats: Option<(f64, f64)>| {
            beats.and_then(|(from, to)| {
                if !from.is_finite() || !to.is_finite() {
                    return None;
                }
                let from = timeline.beat_to_sample_index(from, rate);
                let to = timeline.beat_to_sample_index(to, rate);
                (to > from).then_some((from, to))
            })
        };
        self.loop_samples = bounds(self.loop_beats);
        self.punch_samples = bounds(self.punch_beats);
    }

    /// A new plan fixed the sample rate: keep the musical position and rebuild the bounds.
    pub(crate) fn set_sample_rate(&mut self, sample_rate: u32, timeline: &Timeline) {
        let sample_rate = sample_rate.max(1);
        if sample_rate == self.sample_rate {
            return;
        }
        let beat = timeline.sample_to_beat(self.position, self.sample_rate);
        self.sample_rate = sample_rate;
        self.position = timeline.beat_to_sample_index(beat, sample_rate);
        // A count-in in flight keeps its length in the old rate's samples; drop it.
        self.count_in = None;
        self.refresh_bounds(timeline);
    }

    /// The timeline changed: keep the musical position (the beat), move the sample position
    /// and the loop and punch bounds to the new tempo.
    pub(crate) fn timeline_changed(&mut self, old: &Timeline, new: &Timeline) {
        let beat = old.sample_to_beat(self.position, self.sample_rate);
        self.position = new.beat_to_sample_index(beat, self.sample_rate);
        self.refresh_bounds(new);
    }

    /// Describes the next `frames` frames for the nodes.
    pub(crate) fn begin_block(&self, frames: usize) -> BlockCore {
        let (count_in, count_in_frames) = match &self.count_in {
            Some(count_in) => {
                let left = count_in.total.saturating_sub(count_in.elapsed);
                (Some(*count_in), (left.min(frames as u64)) as usize)
            }
            None => (None, 0),
        };
        let plays_from = (self.running && count_in_frames < frames).then_some(count_in_frames);
        BlockCore {
            frames,
            transport: self.summary(),
            count_in,
            count_in_frames,
            plays_from,
            position: self.position,
            looped: self.loop_samples,
            punch: self.punch_samples,
            armed: self.armed,
            metronome: self.metronome,
        }
    }

    /// Moves the state past the block `block` described.
    pub(crate) fn end_block(&mut self, block: &BlockCore, timeline: &Timeline) {
        if let Some(count_in) = &mut self.count_in {
            count_in.elapsed += block.count_in_frames as u64;
            while count_in.next_click < count_in.beats
                && count_in.click_offset(count_in.next_click, timeline, self.sample_rate)
                    < count_in.elapsed
            {
                count_in.next_click += 1;
            }
            if count_in.elapsed >= count_in.total {
                self.count_in = None;
            }
        }
        if let Some(from) = block.plays_from {
            let played = (block.frames - from) as u64;
            self.position = advance_position(self.position, played, block.looped);
        }
    }
}
