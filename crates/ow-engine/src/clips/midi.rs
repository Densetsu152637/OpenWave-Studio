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
//! Controller events (`ClipContent::Midi::controllers`) are scheduled the same way
//! ([`schedule_controllers`]) and played as `Event::control_change` in frame order with the
//! notes (a controller at the same sample as a note goes first). The player remembers whether
//! a sustain pedal (CC 64, 64 or more is down) it sent is still down and lifts it wherever it
//! releases notes (stop, locate, loop wrap), so notes cannot hang under the pedal. On a
//! locate or loop wrap it also re-sends the last value before the new position of the
//! controllers in [`CHASED`], so the instrument starts in the state the clip would have left it.
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

/// One controller event at an absolute timeline sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledCc {
    /// Timeline sample the event takes effect on.
    pub sample: u64,
    /// Controller number, 0 to 127.
    pub controller: u8,
    /// Value, 0 to 127.
    pub value: u8,
}

/// The controllers a locate or loop wrap re-sends the last value of: mod wheel, volume, pan,
/// expression, sustain pedal and filter cutoff.
pub const CHASED: [u8; 6] = [1, 7, 10, 11, 64, 74];

/// The controller events of `clips` (MIDI clips only, muted clips are silent) at `sample_rate`,
/// sorted by sample; events at one sample keep the order of the clips and of the model. An event
/// at or after its clip's end is dropped.
pub fn schedule_controllers<'a>(
    clips: impl IntoIterator<Item = &'a Clip>,
    timeline: &Timeline,
    sample_rate: u32,
) -> Vec<ScheduledCc> {
    let mut events = Vec::new();
    for clip in clips {
        let ClipContent::Midi { controllers, .. } = &clip.content else {
            continue;
        };
        if clip.muted {
            continue;
        }
        let clip_end = clip.start + clip.length;
        for cc in controllers {
            let beat = clip.start + cc.beat;
            if beat >= clip_end || cc.controller > 127 || cc.value > 127 {
                continue;
            }
            events.push(ScheduledCc {
                sample: timeline.beat_to_sample_index(beat, sample_rate),
                controller: cc.controller,
                value: cc.value,
            });
        }
    }
    // Stable: equal samples keep the order they were added in.
    events.sort_by_key(|e| e.sample);
    events
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
    controllers: Vec<ScheduledCc>,
    /// A sustain pedal-down was sent and not yet lifted.
    pedal: bool,
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
            controllers: Vec::new(),
            pedal: false,
            expected: None,
            held: [0; 128],
        }
    }

    /// Plays `controllers` (as [`schedule_controllers`] returns them) along with the notes.
    pub fn with_controllers(mut self, controllers: Vec<ScheduledCc>) -> Self {
        debug_assert!(controllers.windows(2).all(|w| w[0].sample <= w[1].sample));
        self.controllers = controllers;
        self
    }

    /// The scheduled controller events.
    pub fn controllers(&self) -> &[ScheduledCc] {
        &self.controllers
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
                self.chase(start, span.frame() as u32, out);
            }
            self.expected = Some(end);
            let mut note = self.events.partition_point(|e| e.sample < start);
            let mut cc = self.controllers.partition_point(|c| c.sample < start);
            loop {
                let next_note = self.events.get(note).copied().filter(|e| e.sample < end);
                let next_cc = self.controllers.get(cc).copied().filter(|c| c.sample < end);
                match (next_note, next_cc) {
                    (None, None) => break,
                    // A controller at the same sample as a note goes first.
                    (n, Some(c)) if n.is_none_or(|n| c.sample <= n.sample) => {
                        let offset = (span.frame() as u64 + (c.sample - start)) as u32;
                        cc += 1;
                        self.send_controller(offset, c.controller, c.value, out);
                    }
                    (Some(event), _) => {
                        let offset = (span.frame() as u64 + (event.sample - start)) as u32;
                        note += 1;
                        self.send_note(offset, event, out);
                    }
                    (None, Some(_)) => break, // not reachable: the arm above takes it
                }
            }
        }
        if !playing {
            self.release_all(0, out);
            self.expected = None;
        }
    }

    fn send_note(&mut self, offset: u32, event: ScheduledEvent, out: &mut Vec<Event>) {
        if event.is_on() {
            self.held[usize::from(event.key)] = self.held[usize::from(event.key)].saturating_add(1);
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

    fn send_controller(&mut self, offset: u32, controller: u8, value: u8, out: &mut Vec<Event>) {
        if controller == 64 {
            self.pedal = value >= 64;
        }
        push(out, Event::control_change(offset, controller, value));
    }

    /// Re-sends, at `offset`, the last value before timeline sample `start` of each controller in
    /// [`CHASED`] that has one.
    fn chase(&mut self, start: u64, offset: u32, out: &mut Vec<Event>) {
        let before = self.controllers.partition_point(|c| c.sample < start);
        let mut seen = [false; CHASED.len()];
        let mut missing = CHASED.len();
        for index in (0..before).rev() {
            let cc = self.controllers[index];
            let Some(slot) = CHASED.iter().position(|c| *c == cc.controller) else {
                continue;
            };
            if !seen[slot] {
                seen[slot] = true;
                self.send_controller(offset, cc.controller, cc.value, out);
                missing -= 1;
                if missing == 0 {
                    break;
                }
            }
        }
    }

    /// A pedal lift and note-offs at `offset` for every key still sounding.
    fn release_all(&mut self, offset: u32, out: &mut Vec<Event>) {
        if self.pedal {
            self.pedal = false;
            push(out, Event::control_change(offset, 64, 0));
        }
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
