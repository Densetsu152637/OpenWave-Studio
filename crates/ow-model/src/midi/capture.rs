//! What recording does with MIDI: events become notes, and new notes replace or join the ones
//! already on the track.

use super::MIN_NOTE_LENGTH;
use crate::model::{Controller, Note};

/// One recorded channel message, positioned in beats from the start of the clip it goes into.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecordedEvent {
    /// Position in beats (a negative position is moved to 0).
    pub beat: f64,
    /// Status byte and two data bytes, as received.
    pub bytes: [u8; 3],
}

/// Pairs note-ons with note-offs (a note-on with velocity 0 is a note-off; the oldest sounding
/// note of a key ends first; channels are ignored) and collects control changes.
///
/// A note still sounding at the end is closed at `end_beat`. A note-off with nothing sounding is
/// ignored. Notes are ordered by start, then key; every note lasts at least
/// [`MIN_NOTE_LENGTH`]. Events other than notes and control changes (pitch bend, program change,
/// aftertouch) are not captured.
pub fn notes_from_events(events: &[RecordedEvent], end_beat: f64) -> (Vec<Note>, Vec<Controller>) {
    let mut sorted: Vec<&RecordedEvent> = events.iter().collect();
    sorted.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    let mut sounding: Vec<Vec<(f64, u8)>> = vec![Vec::new(); 128];
    let mut notes = Vec::new();
    let mut controllers = Vec::new();
    let close = |notes: &mut Vec<Note>, key: u8, start: f64, velocity: u8, end: f64| {
        notes.push(Note {
            start,
            length: (end - start).max(MIN_NOTE_LENGTH),
            key,
            velocity,
        });
    };
    for event in sorted {
        let [status, data1, data2] = event.bytes;
        let beat = event.beat.max(0.0);
        match status & 0xF0 {
            0x90 if data2 > 0 => sounding[usize::from(data1 & 0x7F)].push((beat, data2.min(127))),
            0x80 | 0x90 => {
                let key = data1 & 0x7F;
                let held = &mut sounding[usize::from(key)];
                if !held.is_empty() {
                    let (start, velocity) = held.remove(0);
                    close(&mut notes, key, start, velocity, beat);
                }
            }
            0xB0 => controllers.push(Controller {
                beat,
                controller: data1 & 0x7F,
                value: data2 & 0x7F,
            }),
            _ => {}
        }
    }
    let end = end_beat.max(0.0);
    for (key, held) in sounding.into_iter().enumerate() {
        for (start, velocity) in held {
            close(&mut notes, key as u8, start, velocity, end.max(start));
        }
    }
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.key.cmp(&b.key)));
    (notes, controllers)
}

/// A piece of a MIDI clip's content that survives [`carve`]: its place on the timeline and its
/// notes and controllers relative to its own start.
#[derive(Debug, Clone, PartialEq)]
pub struct MidiPiece {
    /// Start on the timeline, in beats.
    pub start: f64,
    /// Length in beats.
    pub length: f64,
    /// Notes relative to `start`.
    pub notes: Vec<Note>,
    /// Controllers relative to `start`.
    pub controllers: Vec<Controller>,
}

/// Removes the timeline range `cut_start..cut_end` from a MIDI clip that starts at `clip_start`
/// and is `clip_length` long, returning what is left: nothing (the clip lies inside the range), a
/// piece before the range, a piece after it, or both. A note that starts before the range keeps
/// its start and is shortened to end at the range's start; a note that starts inside the range
/// is dropped; notes after the range move with their piece. A range that misses the clip returns
/// the clip as one piece.
pub fn carve(
    clip_start: f64,
    clip_length: f64,
    notes: &[Note],
    controllers: &[Controller],
    cut_start: f64,
    cut_end: f64,
) -> Vec<MidiPiece> {
    let clip_end = clip_start + clip_length;
    if cut_end <= clip_start || cut_start >= clip_end || cut_end <= cut_start {
        return vec![MidiPiece {
            start: clip_start,
            length: clip_length,
            notes: notes.to_vec(),
            controllers: controllers.to_vec(),
        }];
    }
    let mut pieces = Vec::new();
    if cut_start > clip_start {
        let length = cut_start - clip_start;
        pieces.push(MidiPiece {
            start: clip_start,
            length,
            notes: notes
                .iter()
                .filter(|n| n.start < length)
                .map(|n| Note {
                    length: n.length.min(length - n.start).max(MIN_NOTE_LENGTH),
                    ..*n
                })
                .collect(),
            controllers: controllers
                .iter()
                .filter(|c| c.beat < length)
                .copied()
                .collect(),
        });
    }
    if cut_end < clip_end {
        let shift = cut_end - clip_start;
        pieces.push(MidiPiece {
            start: cut_end,
            length: clip_end - cut_end,
            notes: notes
                .iter()
                .filter(|n| n.start >= shift)
                .map(|n| Note {
                    start: n.start - shift,
                    ..*n
                })
                .collect(),
            controllers: controllers
                .iter()
                .filter(|c| c.beat >= shift)
                .map(|c| Controller {
                    beat: c.beat - shift,
                    ..*c
                })
                .collect(),
        });
    }
    pieces
}

/// Appends `extra` (moved by `shift` beats) to `into`, keeping the list ordered by start (notes
/// that start together keep their existing order).
pub fn merge_notes(into: &mut Vec<Note>, extra: &[Note], shift: f64) {
    into.extend(extra.iter().map(|n| Note {
        start: (n.start + shift).max(0.0),
        ..*n
    }));
    into.sort_by(|a, b| a.start.total_cmp(&b.start));
}

#[cfg(test)]
mod midi_capture_tests {
    use super::*;

    fn ev(beat: f64, status: u8, d1: u8, d2: u8) -> RecordedEvent {
        RecordedEvent {
            beat,
            bytes: [status, d1, d2],
        }
    }

    #[test]
    fn pairs_notes_and_collects_controllers() {
        let events = [
            ev(1.0, 0x80, 60, 0),
            ev(0.0, 0x90, 60, 100),
            ev(0.5, 0xB0, 64, 127),
            ev(1.0, 0x90, 64, 80),
            ev(1.5, 0x91, 64, 0),
        ];
        let (notes, controllers) = notes_from_events(&events, 4.0);
        assert_eq!(notes.len(), 2);
        assert_eq!(
            (notes[0].key, notes[0].start, notes[0].length),
            (60, 0.0, 1.0)
        );
        assert_eq!(notes[0].velocity, 100);
        assert_eq!(
            (notes[1].key, notes[1].start, notes[1].length),
            (64, 1.0, 0.5)
        );
        assert_eq!(controllers.len(), 1);
        assert_eq!(controllers[0].controller, 64);
    }

    #[test]
    fn held_notes_close_at_the_end_and_stray_offs_are_ignored() {
        let events = [ev(0.5, 0x80, 61, 0), ev(2.0, 0x90, 62, 90)];
        let (notes, _) = notes_from_events(&events, 3.0);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].length, 1.0);
    }

    #[test]
    fn repeated_note_ons_on_one_key_end_in_order() {
        let events = [
            ev(0.0, 0x90, 60, 90),
            ev(0.5, 0x90, 60, 70),
            ev(1.0, 0x80, 60, 0),
            ev(1.5, 0x80, 60, 0),
        ];
        let (notes, _) = notes_from_events(&events, 2.0);
        assert_eq!(notes.len(), 2);
        assert_eq!((notes[0].start, notes[0].length), (0.0, 1.0));
        assert_eq!((notes[1].start, notes[1].length), (0.5, 1.0));
    }

    fn n(start: f64, length: f64) -> Note {
        Note {
            start,
            length,
            key: 60,
            velocity: 90,
        }
    }

    #[test]
    fn carve_splits_a_clip_around_the_cut() {
        let notes = [n(0.0, 3.0), n(2.0, 1.0), n(5.0, 1.0)];
        let pieces = carve(0.0, 8.0, &notes, &[], 2.0, 4.0);
        assert_eq!(pieces.len(), 2);
        assert_eq!((pieces[0].start, pieces[0].length), (0.0, 2.0));
        assert_eq!(pieces[0].notes, [n(0.0, 2.0)]);
        assert_eq!((pieces[1].start, pieces[1].length), (4.0, 4.0));
        assert_eq!(pieces[1].notes, [n(1.0, 1.0)]);
    }

    #[test]
    fn carve_removes_a_covered_clip_and_keeps_an_untouched_one() {
        assert!(carve(2.0, 2.0, &[n(0.0, 1.0)], &[], 1.0, 5.0).is_empty());
        let kept = carve(2.0, 2.0, &[n(0.0, 1.0)], &[], 8.0, 9.0);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].notes, [n(0.0, 1.0)]);
    }

    #[test]
    fn merge_keeps_notes_ordered() {
        let mut notes = vec![n(0.0, 1.0), n(2.0, 1.0)];
        merge_notes(&mut notes, &[n(0.0, 0.5)], 1.0);
        assert_eq!(
            notes.iter().map(|x| x.start).collect::<Vec<_>>(),
            [0.0, 1.0, 2.0]
        );
    }
}
