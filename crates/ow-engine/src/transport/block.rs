//! The nodes-facing view of one block of time: which sample ranges map to which beat ranges,
//! with loop wraps, punch ranges, count-in and metronome clicks resolved to exact frames.
//!
//! A block's frames `0..frames` are laid out as an optional count-in (the first
//! [`count_in_frames`](BlockTiming::count_in_frames) frames, the position held) followed by
//! playback. Playback is cut into [`Span`]s: stretches of consecutive timeline samples. Without
//! a loop wrap there is one span; each wrap of the loop starts a new span at the loop start, so a
//! loop boundary in the middle of a block (or a loop shorter than the block) is just several
//! spans. Later clip and automation stages schedule an event by asking each span for the frame
//! of the beat or sample it cares about ([`Span::frame_of_beat`], [`Span::frame_of_sample`]);
//! every sample of every span is rendered exactly once, so nothing is skipped or doubled at the
//! loop point.

use std::ops::Range;

use super::state::{CountIn, Transport};
use super::timeline::Timeline;

/// The plain-data part of a block's timing: `Copy`, no pointers, published to the DSP threads by
/// the audio thread before each cycle.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BlockCore {
    pub(crate) frames: usize,
    pub(crate) transport: Transport,
    pub(crate) count_in: Option<CountIn>,
    /// Frames at the start of the block spent in the count-in.
    pub(crate) count_in_frames: usize,
    /// Frame at which playback starts, if it does within this block.
    pub(crate) plays_from: Option<usize>,
    /// Timeline position of the first playing frame.
    pub(crate) position: u64,
    /// Loop `[start, end)` in samples.
    pub(crate) looped: Option<(u64, u64)>,
    /// Punch `[in, out)` in samples.
    pub(crate) punch: Option<(u64, u64)>,
    pub(crate) armed: bool,
    pub(crate) metronome: bool,
}

/// One metronome click inside a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Click {
    /// Frame of the block at which the click starts.
    pub frame: usize,
    /// True for the downbeat of a bar.
    pub accent: bool,
    /// True when the click belongs to the count-in rather than to playback.
    pub count_in: bool,
}

/// Timing of one block, for nodes: [`ProcessContext::timing`](crate::ProcessContext::timing).
///
/// `Copy`; every method is allocation-free and lock-free.
#[derive(Clone, Copy)]
pub struct BlockTiming<'a> {
    core: BlockCore,
    timeline: &'a Timeline,
    sample_rate: u32,
}

impl<'a> BlockTiming<'a> {
    pub(crate) fn new(core: BlockCore, timeline: &'a Timeline, sample_rate: u32) -> Self {
        Self {
            core,
            timeline,
            sample_rate,
        }
    }

    /// Frames in the block.
    pub fn frames(&self) -> usize {
        self.core.frames
    }

    /// The transport state at the start of the block.
    pub fn transport(&self) -> Transport {
        self.core.transport
    }

    /// The tempo and time-signature maps in force for this block.
    pub fn timeline(&self) -> &'a Timeline {
        self.timeline
    }

    /// The loop `[start, end)` in samples, if looping is on.
    pub fn loop_range(&self) -> Option<Range<u64>> {
        self.core.looped.map(|(start, end)| start..end)
    }

    /// The punch range `[in, out)` in samples, if punching is on.
    pub fn punch_range(&self) -> Option<Range<u64>> {
        self.core.punch.map(|(from, to)| from..to)
    }

    /// Whether the metronome is switched on for playback (count-in clicks always sound).
    pub fn metronome_enabled(&self) -> bool {
        self.core.metronome
    }

    /// Frames at the start of the block that belong to the count-in: the position holds and
    /// nothing plays. 0 when no count-in is running.
    pub fn count_in_frames(&self) -> usize {
        self.core.count_in_frames
    }

    /// The playing part of the block as a sequence of spans (empty while stopped or counting
    /// in for the whole block). Spans are in frame order and tile
    /// `count_in_frames()..frames()` without gaps.
    pub fn spans(&self) -> Spans<'a> {
        Spans {
            core: self.core,
            timeline: self.timeline,
            sample_rate: self.sample_rate,
            // Without playback in this block the iterator starts at the end and yields nothing.
            frame: self.core.plays_from.unwrap_or(self.core.frames),
            position: self.core.position,
            looped_back: false,
        }
    }

    /// Calls `f` for every metronome click that starts in this block, in frame order: the
    /// count-in clicks (always) and, when the metronome is on, one click per beat of the
    /// time-signature map during playback, accented on the downbeat. A click sits on the frame
    /// its beat rounds to ([`Timeline::beat_to_sample_index`]); a beat that falls on the loop
    /// end is not clicked (the position has already wrapped), one on the loop start is.
    pub fn for_each_click(&self, mut f: impl FnMut(Click)) {
        if let Some(count_in) = &self.core.count_in {
            for index in count_in.next_click..count_in.beats {
                let offset = count_in.click_offset(index, self.timeline, self.sample_rate);
                let frame = offset.saturating_sub(count_in.elapsed);
                if frame >= self.core.count_in_frames as u64 {
                    break;
                }
                f(Click {
                    frame: frame as usize,
                    accent: index % count_in.per_bar == 0,
                    count_in: true,
                });
            }
        }
        if !self.core.metronome {
            return;
        }
        let signatures = self.timeline.signatures();
        for span in self.spans() {
            let start = span.start_sample();
            let end = start.saturating_add(span.frames() as u64);
            // Any beat whose frame is `start` or later lies after the previous sample's beat.
            let lower = self
                .timeline
                .sample_to_beat(start.saturating_sub(1), self.sample_rate);
            let mut grid = signatures.grid_at_or_after(lower);
            loop {
                let sample = self
                    .timeline
                    .beat_to_sample_index(grid.beat, self.sample_rate);
                // At the position ceiling every later beat clamps to the same sample.
                if sample >= end || sample >= ow_model::time::MAX_SAMPLE {
                    break;
                }
                if sample >= start {
                    f(Click {
                        frame: span.frame() + (sample - start) as usize,
                        accent: grid.index == 0,
                        count_in: false,
                    });
                }
                let next = signatures.next_grid(grid);
                if next.beat <= grid.beat {
                    // Beats so large that f64 cannot step them: stop rather than spin.
                    break;
                }
                grid = next;
            }
        }
    }
}

/// A stretch of consecutive timeline samples inside a block.
#[derive(Clone, Copy)]
pub struct Span<'a> {
    timeline: &'a Timeline,
    sample_rate: u32,
    frame: usize,
    frames: usize,
    start_sample: u64,
    looped_back: bool,
    record: Option<(usize, usize)>,
}

impl<'a> Span<'a> {
    /// Frame of the block at which the span starts.
    pub fn frame(&self) -> usize {
        self.frame
    }

    /// Length of the span in frames; at least 1.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Block frames covered, `frame()..frame() + frames()`.
    pub fn frame_range(&self) -> Range<usize> {
        self.frame..self.frame + self.frames
    }

    /// Timeline sample position of the span's first frame.
    pub fn start_sample(&self) -> u64 {
        self.start_sample
    }

    /// True when the span starts at the loop start right after a wrap (as opposed to continuing
    /// from the previous span or from the block's start).
    pub fn is_loop_restart(&self) -> bool {
        self.looped_back
    }

    /// Timeline sample position of block frame `frame` (which should lie in the span).
    pub fn sample_at(&self, frame: usize) -> u64 {
        self.start_sample + (frame - self.frame) as u64
    }

    /// Beat at the span's first frame.
    pub fn start_beat(&self) -> f64 {
        self.timeline
            .sample_to_beat(self.start_sample, self.sample_rate)
    }

    /// Beat just after the span's last frame (the beat the next span would start at without a
    /// wrap). The span covers beats `start_beat()..end_beat()`.
    pub fn end_beat(&self) -> f64 {
        self.timeline
            .sample_to_beat(self.start_sample + self.frames as u64, self.sample_rate)
    }

    /// Beat at block frame `frame` (which should lie in the span).
    pub fn beat_at(&self, frame: usize) -> f64 {
        self.timeline
            .sample_to_beat(self.sample_at(frame), self.sample_rate)
    }

    /// The block frame whose timeline sample is `sample`, if it lies in this span.
    pub fn frame_of_sample(&self, sample: u64) -> Option<usize> {
        let offset = sample.checked_sub(self.start_sample)?;
        (offset < self.frames as u64).then(|| self.frame + offset as usize)
    }

    /// The block frame an event at `beat` falls on, if it lies in this span: the beat's sample
    /// position rounded to the nearest sample ([`Timeline::beat_to_sample_index`]). Each beat
    /// maps to one sample and every sample belongs to one span, so across the spans of a block
    /// (and across blocks) an event is placed exactly once.
    pub fn frame_of_beat(&self, beat: f64) -> Option<usize> {
        self.frame_of_sample(self.timeline.beat_to_sample_index(beat, self.sample_rate))
    }

    /// Beats `start_beat()..end_beat()` this span covers. Events must not be placed by comparing
    /// against this range: a beat belongs to the span whose frames contain its *rounded* sample
    /// ([`frame_of_beat`](Self::frame_of_beat)), which differs from the plain range by up to half
    /// a sample at each edge. Use [`contains_beat`](Self::contains_beat).
    pub fn beat_range(&self) -> Range<f64> {
        self.start_beat()..self.end_beat()
    }

    /// Whether an event at `beat` belongs to this span: exactly when
    /// [`frame_of_beat`](Self::frame_of_beat) is `Some`. Across the spans of all blocks every beat
    /// is contained in exactly one span (or none if the position never plays it).
    pub fn contains_beat(&self, beat: f64) -> bool {
        self.frame_of_beat(beat).is_some()
    }

    /// Block frames of this span during which recording is active: the span when recording is
    /// armed and no punch range is set, its overlap with the punch range otherwise, `None` when
    /// not armed or outside the range.
    pub fn record_frames(&self) -> Option<Range<usize>> {
        self.record.map(|(from, to)| from..to)
    }
}

/// Iterator over the playing spans of a block; see [`BlockTiming::spans`].
#[derive(Clone, Copy)]
pub struct Spans<'a> {
    core: BlockCore,
    timeline: &'a Timeline,
    sample_rate: u32,
    frame: usize,
    position: u64,
    looped_back: bool,
}

impl<'a> Iterator for Spans<'a> {
    type Item = Span<'a>;

    fn next(&mut self) -> Option<Span<'a>> {
        let remaining = self.core.frames.checked_sub(self.frame)?;
        if remaining == 0 {
            return None;
        }
        let mut frames = remaining as u64;
        if let Some((_, end)) = self.core.looped
            && self.position < end
        {
            frames = frames.min(end - self.position);
        }
        let start = self.position;
        let record = self.record_range(start, frames);
        let span = Span {
            timeline: self.timeline,
            sample_rate: self.sample_rate,
            frame: self.frame,
            frames: frames as usize,
            start_sample: start,
            looped_back: self.looped_back,
            record,
        };
        self.frame += frames as usize;
        self.position = start + frames;
        self.looped_back = false;
        if let Some((loop_start, loop_end)) = self.core.looped
            && self.position == loop_end
        {
            self.position = loop_start;
            self.looped_back = true;
        }
        Some(span)
    }
}

impl Spans<'_> {
    fn record_range(&self, start: u64, frames: u64) -> Option<(usize, usize)> {
        if !self.core.armed {
            return None;
        }
        let end = start + frames;
        let (from, to) = match self.core.punch {
            Some((punch_in, punch_out)) => (start.max(punch_in), end.min(punch_out)),
            None => (start, end),
        };
        (from < to).then(|| {
            (
                self.frame + (from - start) as usize,
                self.frame + (to - start) as usize,
            )
        })
    }
}
