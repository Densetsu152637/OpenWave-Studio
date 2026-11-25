//! What a MIDI recording does to the track it was recorded on: [`plan_recording`] turns the
//! recorded passes into clip changes for one of three [`MidiRecordMode`]s. Pure: it returns the
//! clips to remove, replace and add; the caller turns them into edit commands (one transaction).

use serde::{Deserialize, Serialize};

use super::capture::{RecordedEvent, carve, merge_notes, notes_from_events};
use crate::ids::{ClipId, TrackId};
use crate::model::{Clip, ClipContent, Controller, Note};

/// How a MIDI recording meets what is already on the track.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MidiRecordMode {
    /// The recording is a new clip and replaces the MIDI clips under its range (clips that
    /// straddle the range are cut). With a loop every lap is a clip of its own over the loop,
    /// the last one playing and the earlier ones muted (unmute one and mute the others to
    /// choose a take; MIDI clips are not `take` groups, which are for audio).
    #[default]
    Replace,
    /// The recorded notes join the MIDI clip they fall in (every lap of a loop adds to it);
    /// notes where there is no clip go into a new clip over the recorded range.
    Overdub,
    /// A loop recording merges every lap into one new clip that replaces what is under the
    /// loop. Without a loop it is the same as `replace`.
    LoopMerge,
}

/// One recorded pass: its range on the timeline and the messages in it, positions in beats.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedPass {
    /// First recorded beat.
    pub start: f64,
    /// Beat just after the last recorded frame.
    pub end: f64,
    /// Messages at absolute timeline beats (a position before `start` counts as `start`).
    pub events: Vec<RecordedEvent>,
}

/// The clip changes a recording makes, to apply in this order: remove, replace, add.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecordingClips {
    /// Clips that were wholly under the recorded range.
    pub remove: Vec<ClipId>,
    /// Existing clips (same ids) that were cut or received notes.
    pub replace: Vec<Clip>,
    /// New clips.
    pub add: Vec<Clip>,
}

/// Plans the clips for `passes` (in pass order) recorded on `track`. `existing` are the MIDI
/// clips already on the track, `looped` says the recording looped, `name` names new clips and
/// `alloc` hands out fresh clip ids. A pass without notes and controllers adds nothing; a recording
/// without any changes nothing.
pub fn plan_recording<E>(
    existing: &[Clip],
    track: TrackId,
    name: &str,
    passes: &[RecordedPass],
    mode: MidiRecordMode,
    looped: bool,
    alloc: &mut dyn FnMut() -> Result<ClipId, E>,
) -> Result<RecordingClips, E> {
    let existing: Vec<&Clip> = existing
        .iter()
        .filter(|c| c.track == track && matches!(c.content, ClipContent::Midi { .. }))
        .collect();
    let mut out = RecordingClips::default();
    let contents: Vec<(f64, f64, Vec<Note>, Vec<Controller>)> = passes
        .iter()
        .map(|pass| {
            let length = (pass.end - pass.start).max(0.0);
            let relative: Vec<RecordedEvent> = pass
                .events
                .iter()
                .map(|e| RecordedEvent {
                    beat: (e.beat - pass.start).max(0.0),
                    bytes: e.bytes,
                })
                .collect();
            let (notes, controllers) = notes_from_events(&relative, length);
            (pass.start, length, notes, controllers)
        })
        .collect();
    let any = contents
        .iter()
        .any(|(_, _, notes, ccs)| !notes.is_empty() || !ccs.is_empty());
    if !any {
        return Ok(out);
    }

    match mode {
        MidiRecordMode::Overdub => {
            let mut merged: Vec<(usize, Vec<Note>, Vec<Controller>)> = Vec::new();
            let mut leftover_notes = Vec::new();
            let mut leftover_controllers = Vec::new();
            for (start, _, notes, controllers) in &contents {
                for note in notes {
                    let at = start + note.start;
                    match existing_at(&existing, at) {
                        Some(index) => {
                            let clip = existing[index];
                            let slot = merged_slot(&mut merged, index, clip);
                            merge_notes(
                                &mut merged[slot].1,
                                &[Note {
                                    start: at - clip.start,
                                    ..*note
                                }],
                                0.0,
                            );
                        }
                        None => leftover_notes.push(Note { start: at, ..*note }),
                    }
                }
                for cc in controllers {
                    let at = start + cc.beat;
                    match existing_at(&existing, at) {
                        Some(index) => {
                            let clip = existing[index];
                            let slot = merged_slot(&mut merged, index, clip);
                            merged[slot].2.push(Controller {
                                beat: at - clip.start,
                                ..*cc
                            });
                        }
                        None => leftover_controllers.push(Controller { beat: at, ..*cc }),
                    }
                }
            }
            for (index, notes, controllers) in merged {
                let mut clip = existing[index].clone();
                clip.content = ClipContent::Midi { notes, controllers };
                out.replace.push(clip);
            }
            if !leftover_notes.is_empty() || !leftover_controllers.is_empty() {
                let first = contents.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
                let last = contents.iter().map(|c| c.0 + c.1).fold(0.0_f64, f64::max);
                let mut notes = leftover_notes;
                for note in &mut notes {
                    note.start -= first;
                }
                for cc in &mut leftover_controllers {
                    cc.beat -= first;
                }
                notes.sort_by(|a, b| a.start.total_cmp(&b.start));
                out.add.push(new_clip(
                    alloc()?,
                    track,
                    name,
                    first,
                    last - first,
                    notes,
                    leftover_controllers,
                ));
            }
        }
        MidiRecordMode::LoopMerge | MidiRecordMode::Replace => {
            let merge = mode == MidiRecordMode::LoopMerge && contents.len() > 1;
            let groups: Vec<(f64, f64, Vec<Note>, Vec<Controller>)> = if merge || !looped {
                // One clip: the laps merged into the range they cover, or the single pass.
                let first = contents.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
                let last = contents.iter().map(|c| c.0 + c.1).fold(0.0_f64, f64::max);
                let mut notes = Vec::new();
                let mut controllers = Vec::new();
                for (start, _, n, c) in &contents {
                    merge_notes(&mut notes, n, start - first);
                    controllers.extend(c.iter().map(|cc| Controller {
                        beat: cc.beat + start - first,
                        ..*cc
                    }));
                }
                controllers.sort_by(|a, b| a.beat.total_cmp(&b.beat));
                vec![(first, last - first, notes, controllers)]
            } else {
                contents.clone()
            };
            // Everything under the recorded range goes (once for the union of the passes).
            let cut_start = groups.iter().map(|g| g.0).fold(f64::INFINITY, f64::min);
            let cut_end = groups.iter().map(|g| g.0 + g.1).fold(0.0_f64, f64::max);
            for clip in &existing {
                let ClipContent::Midi { notes, controllers } = &clip.content else {
                    continue;
                };
                let end = clip.start + clip.length;
                if end <= cut_start || clip.start >= cut_end {
                    continue;
                }
                let mut pieces = carve(
                    clip.start,
                    clip.length,
                    notes,
                    controllers,
                    cut_start,
                    cut_end,
                )
                .into_iter();
                match pieces.next() {
                    None => out.remove.push(clip.id),
                    Some(first) => {
                        let mut kept = (*clip).clone();
                        kept.start = first.start;
                        kept.length = first.length;
                        kept.content = ClipContent::Midi {
                            notes: first.notes,
                            controllers: first.controllers,
                        };
                        out.replace.push(kept);
                        for piece in pieces {
                            let mut extra = (*clip).clone();
                            extra.id = alloc()?;
                            extra.start = piece.start;
                            extra.length = piece.length;
                            extra.content = ClipContent::Midi {
                                notes: piece.notes,
                                controllers: piece.controllers,
                            };
                            out.add.push(extra);
                        }
                    }
                }
            }
            let laps = looped && !merge && groups.len() > 1;
            let count = groups.len();
            for (index, (start, length, notes, controllers)) in groups.into_iter().enumerate() {
                let id = alloc()?;
                let label = if laps {
                    format!("{name} take {}", index + 1)
                } else {
                    name.to_string()
                };
                let mut clip = new_clip(id, track, &label, start, length, notes, controllers);
                // Every lap is kept; only the last one plays until the user chooses another.
                clip.muted = laps && index + 1 < count;
                out.add.push(clip);
            }
        }
    }
    Ok(out)
}

/// The existing clip (by index) whose range holds `beat`, the last one when several overlap.
fn existing_at(existing: &[&Clip], beat: f64) -> Option<usize> {
    existing
        .iter()
        .rposition(|c| beat >= c.start && beat < c.start + c.length)
}

type Merged = (usize, Vec<Note>, Vec<Controller>);

/// The entry of `merged` for existing clip `index`, created from the clip's own content.
fn merged_slot(merged: &mut Vec<Merged>, index: usize, clip: &Clip) -> usize {
    if let Some(slot) = merged.iter().position(|m| m.0 == index) {
        return slot;
    }
    let (notes, controllers) = match &clip.content {
        ClipContent::Midi { notes, controllers } => (notes.clone(), controllers.clone()),
        _ => (Vec::new(), Vec::new()),
    };
    merged.push((index, notes, controllers));
    merged.len() - 1
}

fn new_clip(
    id: ClipId,
    track: TrackId,
    name: &str,
    start: f64,
    length: f64,
    notes: Vec<Note>,
    controllers: Vec<Controller>,
) -> Clip {
    Clip {
        id,
        track,
        name: name.to_string(),
        start,
        length: length.max(super::MIN_NOTE_LENGTH),
        muted: false,
        content: ClipContent::Midi { notes, controllers },
        take: None,
    }
}

#[cfg(test)]
mod midi_record_tests {
    use std::convert::Infallible;

    use super::*;

    fn note(start: f64, length: f64, key: u8) -> Note {
        Note {
            start,
            length,
            key,
            velocity: 100,
        }
    }

    fn clip(id: u64, start: f64, length: f64, notes: Vec<Note>) -> Clip {
        Clip {
            id: ClipId(id),
            track: TrackId(1),
            name: format!("clip {id}"),
            start,
            length,
            muted: false,
            content: ClipContent::Midi {
                notes,
                controllers: vec![],
            },
            take: None,
        }
    }

    fn on_off(beat: f64, length: f64, key: u8) -> [RecordedEvent; 2] {
        [
            RecordedEvent {
                beat,
                bytes: [0x90, key, 90],
            },
            RecordedEvent {
                beat: beat + length,
                bytes: [0x80, key, 0],
            },
        ]
    }

    /// A pass from `start` to `end` in which notes `(beat after start, length, key)` were played.
    fn pass(start: f64, end: f64, played: &[(f64, f64, u8)]) -> RecordedPass {
        RecordedPass {
            start,
            end,
            events: played
                .iter()
                .flat_map(|(beat, length, key)| on_off(start + *beat, *length, *key))
                .collect(),
        }
    }

    fn plan(
        existing: &[Clip],
        passes: &[RecordedPass],
        mode: MidiRecordMode,
        looped: bool,
    ) -> RecordingClips {
        let mut next = 100;
        let mut alloc = || -> Result<ClipId, Infallible> {
            next += 1;
            Ok(ClipId(next))
        };
        plan_recording(
            existing,
            TrackId(1),
            "Take",
            passes,
            mode,
            looped,
            &mut alloc,
        )
        .unwrap()
    }

    fn notes_of(clip: &Clip) -> Vec<Note> {
        match &clip.content {
            ClipContent::Midi { notes, .. } => notes.clone(),
            _ => panic!("not MIDI"),
        }
    }

    #[test]
    fn replace_cuts_the_clips_under_the_range_and_adds_one_clip() {
        let existing = [
            clip(
                1,
                0.0,
                8.0,
                vec![note(0.0, 1.0, 60), note(3.0, 1.0, 62), note(6.0, 1.0, 64)],
            ),
            clip(2, 9.0, 2.0, vec![note(0.0, 1.0, 65)]),
            clip(3, 5.0, 2.0, vec![note(0.0, 1.0, 66)]),
        ];
        // Recorded beats 2 to 6: clip 1 is cut in two, clip 3 (5 to 7) is cut to 6 to 7 and
        // clip 2 (9 to 11) is untouched.
        let out = plan(
            &existing,
            &[pass(2.0, 6.0, &[(0.5, 1.0, 72)])],
            MidiRecordMode::Replace,
            false,
        );
        assert!(out.remove.is_empty());
        let replaced: Vec<(u64, f64, f64)> = out
            .replace
            .iter()
            .map(|c| (c.id.0, c.start, c.length))
            .collect();
        assert_eq!(replaced, [(1, 0.0, 2.0), (3, 6.0, 1.0)]);
        // The cut clip keeps the note before the range and its right piece the note after.
        assert_eq!(notes_of(&out.replace[0]), [note(0.0, 1.0, 60)]);
        assert!(
            notes_of(&out.replace[1]).is_empty(),
            "the note at beat 5 was in the range"
        );
        let added: Vec<(f64, f64)> = out.add.iter().map(|c| (c.start, c.length)).collect();
        assert_eq!(
            added,
            [(6.0, 2.0), (2.0, 4.0)],
            "the right piece, then the recording"
        );
        assert_eq!(out.add[0].id, ClipId(101));
        assert_eq!(notes_of(&out.add[0]), [note(0.0, 1.0, 64)]);
        // Played at velocity 90 (see `on_off`).
        assert_eq!(
            notes_of(&out.add[1]),
            [Note {
                velocity: 90,
                ..note(0.5, 1.0, 72)
            }]
        );
        assert!(out.add[1].take.is_none());
    }

    #[test]
    fn replace_removes_a_clip_that_lies_inside_the_range() {
        let existing = [clip(1, 3.0, 1.0, vec![note(0.0, 1.0, 60)])];
        let out = plan(
            &existing,
            &[pass(2.0, 6.0, &[(0.0, 1.0, 72)])],
            MidiRecordMode::Replace,
            false,
        );
        assert_eq!(out.remove, [ClipId(1)]);
        assert_eq!(out.add.len(), 1);
    }

    #[test]
    fn replace_with_a_loop_keeps_every_lap_as_a_clip_and_only_the_last_plays() {
        let passes = [
            pass(4.0, 8.0, &[(0.0, 1.0, 60)]),
            pass(4.0, 8.0, &[(1.0, 1.0, 62)]),
            pass(4.0, 8.0, &[(2.0, 1.0, 64)]),
        ];
        let out = plan(&[], &passes, MidiRecordMode::Replace, true);
        assert_eq!(out.add.len(), 3);
        assert!(out.add.iter().all(|c| c.take.is_none()));
        assert_eq!(
            out.add.iter().map(|c| c.muted).collect::<Vec<_>>(),
            [true, true, false]
        );
        assert!(out.add.iter().all(|c| (c.start, c.length) == (4.0, 4.0)));
        assert_eq!(out.add[2].name, "Take take 3");
    }

    #[test]
    fn overdub_merges_into_the_clip_under_each_note() {
        let existing = [
            clip(1, 0.0, 4.0, vec![note(0.0, 1.0, 60)]),
            clip(2, 4.0, 4.0, vec![note(2.0, 1.0, 62)]),
        ];
        let out = plan(
            &existing,
            &[pass(
                2.0,
                8.0,
                &[(0.0, 1.0, 70), (3.0, 1.0, 71), (5.0, 0.5, 72)],
            )],
            MidiRecordMode::Overdub,
            false,
        );
        assert!(out.add.is_empty() && out.remove.is_empty());
        assert_eq!(out.replace.len(), 2);
        // The notes sound at beats 2, 5 and 7: the first is in clip 1, the others in clip 2.
        let first = notes_of(&out.replace[0]);
        assert_eq!(
            first.iter().map(|n| (n.start, n.key)).collect::<Vec<_>>(),
            [(0.0, 60), (2.0, 70)]
        );
        let second = notes_of(&out.replace[1]);
        assert_eq!(
            second.iter().map(|n| (n.start, n.key)).collect::<Vec<_>>(),
            [(1.0, 71), (2.0, 62), (3.0, 72)]
        );
        // The clips themselves did not move or change length.
        assert_eq!((out.replace[0].start, out.replace[0].length), (0.0, 4.0));
    }

    #[test]
    fn overdub_puts_notes_with_no_clip_in_a_new_clip_and_merges_every_lap() {
        let existing = [clip(1, 0.0, 4.0, vec![])];
        let passes = [
            pass(2.0, 6.0, &[(0.0, 1.0, 60), (3.0, 0.5, 61)]),
            pass(2.0, 6.0, &[(1.0, 1.0, 62)]),
        ];
        let out = plan(&existing, &passes, MidiRecordMode::Overdub, true);
        // Lap one plays at beats 2 (in clip 1) and 5 (outside); lap two at beat 3 (in clip 1).
        assert_eq!(notes_of(&out.replace[0]).len(), 2);
        assert_eq!(out.add.len(), 1);
        assert_eq!((out.add[0].start, out.add[0].length), (2.0, 4.0));
        assert_eq!(
            notes_of(&out.add[0])[0].start,
            3.0,
            "relative to the new clip"
        );
    }

    #[test]
    fn loop_merge_joins_every_lap_into_one_clip_that_replaces_the_range() {
        let existing = [clip(
            1,
            0.0,
            8.0,
            vec![note(0.0, 1.0, 60), note(7.0, 1.0, 61)],
        )];
        let passes = [
            pass(2.0, 4.0, &[(0.0, 0.5, 70)]),
            pass(2.0, 4.0, &[(1.0, 0.5, 71)]),
            pass(2.0, 4.0, &[(0.5, 0.25, 72)]),
        ];
        let out = plan(&existing, &passes, MidiRecordMode::LoopMerge, true);
        let recorded = out.add.last().unwrap();
        assert!(recorded.take.is_none(), "one clip, not takes");
        assert_eq!((recorded.start, recorded.length), (2.0, 2.0));
        let keys: Vec<u8> = notes_of(recorded).iter().map(|n| n.key).collect();
        assert_eq!(keys, [70, 72, 71], "ordered by start");
        // The old clip was cut around the loop.
        assert_eq!((out.replace[0].start, out.replace[0].length), (0.0, 2.0));
        assert_eq!((out.add[0].start, out.add[0].length), (4.0, 4.0));
    }

    #[test]
    fn an_empty_recording_changes_nothing() {
        let existing = [clip(1, 0.0, 8.0, vec![note(0.0, 1.0, 60)])];
        for mode in [
            MidiRecordMode::Replace,
            MidiRecordMode::Overdub,
            MidiRecordMode::LoopMerge,
        ] {
            let out = plan(&existing, &[pass(2.0, 4.0, &[])], mode, false);
            assert_eq!(out, RecordingClips::default());
        }
    }

    #[test]
    fn modes_parse_from_their_kebab_case_names() {
        let mode: MidiRecordMode = serde_json::from_str("\"loop-merge\"").unwrap();
        assert_eq!(mode, MidiRecordMode::LoopMerge);
        assert_eq!(MidiRecordMode::default(), MidiRecordMode::Replace);
    }
}
