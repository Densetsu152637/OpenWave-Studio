//! MIDI clips as timeline-driven note events.
//!
//! [`schedule`] turns a track's MIDI clips into a sorted list of [`ScheduledEvent`]s at absolute
//! timeline samples (on the control thread, at compile). A [`MidiClipPlayer`] then walks that
//! list inside `process`, like `ClipSourceNode` walks its placements: for each span of the
//! block ([`BlockTiming::spans`](crate::BlockTiming::spans)) it emits the events whose sample
//! falls in the span, with the frame inside the block as their offset. Beats map to samples
//! with [`Timeline::beat_to_sample_index`], the same rounding every other timeline event uses,
//! so a note starts on exactly the sample its beat maps to, across block sizes, loops and tempo
//! changes.
//!
//! The player also keeps the output tidy when time is not continuous: it emits a note-off for
//! every sounding note when the transport stops, on a locate (the next span does not start where
//! the last one ended) and at a loop wrap, so no note hangs. Overlapping notes on the same key
//! count as one sounding note per note-on and are released together by the first note-off
//! (synth voices release by key).
//!
//! `process` never allocates: events go into a buffer the caller sized in advance; if it is
//! full the newest events are dropped (a block would need thousands of simultaneous events).

use ow_dsp::{Event, EventKind};
use ow_model::{Clip, ClipContent};

use crate::transport::{BlockTiming, Timeline};

/// One note event at an absolute timeline sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledEvent {
    /// Timeline sample the event takes effect on.
    pub sample: u64,
    /// MIDI key, 0 to 127.
    pub key: u8,
    /// Velocity for a note-on, 0 for a note-off.
    pub velocity: u8,
}

impl ScheduledEvent {
    /// True for a note-on.
    pub fn is_on(&self) -> bool {
        self.velocity > 0
    }
}

/// The note events of `clips` (MIDI clips only; audio and pattern clips are ignored, muted clips
/// are silent) at `sample_rate`, sorted by sample with note-offs before note-ons at the same
/// sample (so a repeated key retriggers). A note is cut at its clip's end; a note that starts at
/// or after the clip's end is dropped; every kept note lasts at least one sample.
pub fn schedule<'a>(
    clips: impl IntoIterator<Item = &'a Clip>,
    timeline: &Timeline,
    sample_rate: u32,
) -> Vec<ScheduledEvent> {
    let mut events = Vec::new();
    for clip in clips {
        let ClipContent::Midi { notes, .. } = &clip.content else {
            continue;
        };
        if clip.muted {
            continue;
        }
        let clip_end = clip.start + clip.length;
        for note in notes {
            let on_beat = clip.start + note.start;
            if on_beat >= clip_end || note.key > 127 {
                continue;
            }
            let off_beat = (on_beat + note.length).min(clip_end);
            let on = timeline.beat_to_sample_index(on_beat, sample_rate);
            let off = timeline
                .beat_to_sample_index(off_beat, sample_rate)
                .max(on.saturating_add(1));
            events.push(ScheduledEvent {
                sample: on,
                key: note.key,
                velocity: note.velocity.clamp(1, 127),
            });
            events.push(ScheduledEvent {
                sample: off,
                key: note.key,
                velocity: 0,
            });
        }
    }
    events.sort_by_key(|e| (e.sample, e.is_on(), e.key));
    events
}

/// Emits a track's note events block by block; see the [module documentation](self).
#[derive(Debug, Clone)]
pub struct MidiClipPlayer {
    events: Vec<ScheduledEvent>,
    /// The timeline sample the next span is expected to start at; `None` while stopped.
    expected: Option<u64>,
    /// Note-ons not yet matched by a note-off, per key.
    held: [u8; 128],
}

impl MidiClipPlayer {
    /// A player for `events` (as [`schedule`] returns them: sorted by sample).
    pub fn new(events: Vec<ScheduledEvent>) -> Self {
        debug_assert!(events.windows(2).all(|w| w[0].sample <= w[1].sample));
        Self {
            events,
            expected: None,
            held: [0; 128],
        }
    }

    /// The scheduled events.
    pub fn events(&self) -> &[ScheduledEvent] {
        &self.events
    }

    /// Appends to `out` (which the caller cleared and sized) the events of the block `timing`
    /// describes, in frame order. Allocation-free.
    pub fn generate(&mut self, timing: &BlockTiming<'_>, out: &mut Vec<Event>) {
        let mut playing = false;
        for span in timing.spans() {
            playing = true;
            let start = span.start_sample();
            let end = start.saturating_add(span.frames() as u64);
            if self.expected != Some(start) {
                self.release_all(span.frame() as u32, out);
            }
            self.expected = Some(end);
            let first = self.events.partition_point(|e| e.sample < start);
            for event in &self.events[first..] {
                if event.sample >= end {
                    break;
                }
                let offset = (span.frame() as u64 + (event.sample - start)) as u32;
                if event.is_on() {
                    self.held[usize::from(event.key)] =
                        self.held[usize::from(event.key)].saturating_add(1);
                    push(
                        out,
                        Event {
                            offset,
                            kind: EventKind::NoteOn {
                                key: event.key,
                                velocity: event.velocity,
                            },
                        },
                    );
                } else {
                    self.held[usize::from(event.key)] = 0;
                    push(out, Event::note_off(offset, event.key));
                }
            }
        }
        if !playing {
            self.release_all(0, out);
            self.expected = None;
        }
    }

    /// Note-offs at `offset` for every key still sounding.
    fn release_all(&mut self, offset: u32, out: &mut Vec<Event>) {
        for key in 0..128_u8 {
            if self.held[usize::from(key)] > 0 {
                self.held[usize::from(key)] = 0;
                push(out, Event::note_off(offset, key));
            }
        }
    }
}

/// Pushes without growing: a full buffer drops the event.
fn push(out: &mut Vec<Event>, event: Event) {
    if out.len() < out.capacity() {
        out.push(event);
    }
}
