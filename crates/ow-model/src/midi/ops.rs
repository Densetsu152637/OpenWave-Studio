//! Note operations: quantize, legato, humanize, transpose and velocity changes.
//!
//! Each function takes a note list and returns a new one of the same order (transpose drops notes
//! that would leave the MIDI range). [`MidiEdit::apply`] runs one operation, optionally on a
//! selection of note indices only; the notes outside the selection are returned unchanged.

use serde::{Deserialize, Serialize};

use super::MIN_NOTE_LENGTH;
use crate::model::Note;

/// What [`legato`] stretches each note up to.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegatoMode {
    /// The next note start on any key (a melody line: chords share the next onset).
    #[default]
    AnyKey,
    /// The next start of a note on the same key (repeated notes join up, other keys are left alone).
    SameKey,
}

/// One note operation, as the MCP tool `midi_edit` and the piano roll name them.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MidiEdit {
    /// Moves note starts toward the grid. See [`quantize`].
    Quantize {
        /// Grid size in beats (0.25 is a sixteenth note).
        grid: f64,
        /// 0 leaves notes where they are, 1 puts them on the grid (default 1).
        #[serde(default = "one")]
        strength: f64,
        /// Also quantize note lengths to whole grid steps (default false).
        #[serde(default)]
        lengths: bool,
    },
    /// Stretches each note to the next note start. See [`legato`].
    Legato {
        /// Which notes count as "next" (default `any_key`).
        #[serde(default)]
        mode: LegatoMode,
    },
    /// Random, repeatable timing and velocity changes. See [`humanize`].
    Humanize {
        /// The same seed gives the same result.
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        seed: u64,
        /// Largest start shift in beats, either direction (default 0.02).
        #[serde(default = "default_timing")]
        timing: f64,
        /// Largest velocity change, either direction (default 8).
        #[serde(default = "default_velocity")]
        velocity: u8,
    },
    /// Moves notes by semitones; notes that would leave 0 to 127 are removed.
    Transpose {
        /// Semitones, positive up.
        semitones: i32,
    },
    /// Multiplies velocities (limited to 1 to 127).
    VelocityScale {
        /// Factor, for example 0.8.
        factor: f64,
    },
    /// Sets every velocity.
    VelocitySet {
        /// Velocity, 1 to 127.
        velocity: u8,
    },
}

fn one() -> f64 {
    1.0
}

fn default_timing() -> f64 {
    0.02
}

fn default_velocity() -> u8 {
    8
}

impl MidiEdit {
    /// The undo-menu name of the operation.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Quantize { .. } => "Quantize",
            Self::Legato { .. } => "Legato",
            Self::Humanize { .. } => "Humanize",
            Self::Transpose { .. } => "Transpose",
            Self::VelocityScale { .. } | Self::VelocitySet { .. } => "Change velocity",
        }
    }

    /// Applies the operation to `notes`, or only to the notes at the indices in `selection`.
    /// Out-of-range and repeated indices are ignored. The result keeps the order of `notes`
    /// (without removed notes).
    pub fn apply(&self, notes: &[Note], selection: Option<&[usize]>) -> Vec<Note> {
        let Some(selection) = selection else {
            return self.run(notes).into_iter().map(|(_, n)| n).collect();
        };
        let mut chosen = vec![false; notes.len()];
        for &index in selection {
            if let Some(slot) = chosen.get_mut(index) {
                *slot = true;
            }
        }
        let picked: Vec<Note> = notes
            .iter()
            .zip(&chosen)
            .filter(|(_, c)| **c)
            .map(|(n, _)| *n)
            .collect();
        // Operations may drop notes, so match results back by the picked note each came from.
        let edited = self.run(&picked);
        let mut out = Vec::with_capacity(notes.len());
        let mut next = 0;
        let mut picked_index = 0;
        for (note, c) in notes.iter().zip(&chosen) {
            if *c {
                if let Some(&(origin, changed)) = edited.get(next)
                    && origin == picked_index
                {
                    out.push(changed);
                    next += 1;
                }
                picked_index += 1;
            } else {
                out.push(*note);
            }
        }
        out
    }

    /// The edited notes with the index of the input note each came from.
    fn run(&self, notes: &[Note]) -> Vec<(usize, Note)> {
        let same_count = |out: Vec<Note>| out.into_iter().enumerate().collect();
        match self {
            Self::Quantize {
                grid,
                strength,
                lengths,
            } => same_count(quantize(notes, *grid, *strength, *lengths)),
            Self::Legato { mode } => same_count(legato(notes, *mode)),
            Self::Humanize {
                seed,
                timing,
                velocity,
            } => same_count(humanize(notes, *seed, *timing, *velocity)),
            Self::Transpose { semitones } => notes
                .iter()
                .enumerate()
                .filter_map(|(i, n)| transpose_note(n, *semitones).map(|n| (i, n)))
                .collect(),
            Self::VelocityScale { factor } => same_count(scale_velocity(notes, *factor)),
            Self::VelocitySet { velocity } => same_count(set_velocity(notes, *velocity)),
        }
    }
}

/// Moves each note start toward the nearest grid line by `strength` (0 to 1; values outside are
/// limited). With `lengths`, each length also moves toward the nearest whole number of grid steps
/// (at least one). A grid that is not a positive number leaves the notes unchanged.
pub fn quantize(notes: &[Note], grid: f64, strength: f64, lengths: bool) -> Vec<Note> {
    if !(grid.is_finite() && grid > 0.0) {
        return notes.to_vec();
    }
    let strength = if strength.is_finite() {
        strength.clamp(0.0, 1.0)
    } else {
        1.0
    };
    notes
        .iter()
        .map(|note| {
            let target = (note.start / grid).round() * grid;
            let mut out = *note;
            out.start = (note.start + (target - note.start) * strength).max(0.0);
            if lengths {
                let steps = (note.length / grid).round().max(1.0);
                out.length =
                    (note.length + (steps * grid - note.length) * strength).max(MIN_NOTE_LENGTH);
            }
            out
        })
        .collect()
}

/// Sets each note's length so it ends where the next note starts (extending short notes and
/// trimming overlapping ones). "Next" is the nearest later start on any key, or on the same key
/// ([`LegatoMode`]); starts within a thousandth of a beat count as simultaneous. A note with no
/// later note keeps its length.
pub fn legato(notes: &[Note], mode: LegatoMode) -> Vec<Note> {
    const SAME: f64 = 0.001;
    notes
        .iter()
        .map(|note| {
            let next = notes
                .iter()
                .filter(|other| mode == LegatoMode::AnyKey || other.key == note.key)
                .map(|other| other.start)
                .filter(|start| *start > note.start + SAME)
                .fold(f64::INFINITY, f64::min);
            let mut out = *note;
            if next.is_finite() {
                out.length = (next - note.start).max(MIN_NOTE_LENGTH);
            }
            out
        })
        .collect()
}

/// Shifts each note start by a random amount of up to `timing` beats and each velocity by up to
/// `velocity`, from a generator seeded with `seed` and the note's index: the same input and seed
/// always give the same result. Starts stay at 0 or later; velocities stay 1 to 127.
pub fn humanize(notes: &[Note], seed: u64, timing: f64, velocity: u8) -> Vec<Note> {
    let timing = if timing.is_finite() {
        timing.abs()
    } else {
        0.0
    };
    notes
        .iter()
        .enumerate()
        .map(|(i, note)| {
            let mut state = seed ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let dt = unit(&mut state) * timing;
            let dv = (unit(&mut state) * f64::from(velocity)).round() as i16;
            let mut out = *note;
            out.start = (note.start + dt).max(0.0);
            out.velocity = (i16::from(note.velocity) + dv).clamp(1, 127) as u8;
            out
        })
        .collect()
}

/// SplitMix64 step mapped to `[-1, 1]`.
fn unit(state: &mut u64) -> f64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1_u64 << 52) as f64 - 1.0
}

fn transpose_note(note: &Note, semitones: i32) -> Option<Note> {
    let key = i64::from(note.key) + i64::from(semitones);
    (0..=127).contains(&key).then_some(Note {
        key: key as u8,
        ..*note
    })
}

/// Moves every note by `semitones`; notes that would leave 0 to 127 are removed.
pub fn transpose(notes: &[Note], semitones: i32) -> Vec<Note> {
    notes
        .iter()
        .filter_map(|n| transpose_note(n, semitones))
        .collect()
}

/// Multiplies every velocity by `factor`, rounded and limited to 1 to 127. A factor that is not
/// a non-negative number leaves the notes unchanged.
pub fn scale_velocity(notes: &[Note], factor: f64) -> Vec<Note> {
    if !(factor.is_finite() && factor >= 0.0) {
        return notes.to_vec();
    }
    notes
        .iter()
        .map(|n| Note {
            velocity: (f64::from(n.velocity) * factor).round().clamp(1.0, 127.0) as u8,
            ..*n
        })
        .collect()
}

/// Sets every velocity (limited to 1 to 127).
pub fn set_velocity(notes: &[Note], velocity: u8) -> Vec<Note> {
    notes
        .iter()
        .map(|n| Note {
            velocity: velocity.clamp(1, 127),
            ..*n
        })
        .collect()
}

#[cfg(test)]
mod midi_ops_tests {
    use super::*;

    fn note(start: f64, length: f64, key: u8, velocity: u8) -> Note {
        Note {
            start,
            length,
            key,
            velocity,
        }
    }

    #[test]
    fn quantize_full_strength_snaps_to_the_grid() {
        let notes = [note(0.12, 0.4, 60, 100), note(0.9, 0.2, 62, 100)];
        let out = quantize(&notes, 0.25, 1.0, false);
        assert_eq!(out[0].start, 0.0);
        assert_eq!(out[1].start, 1.0);
        assert_eq!(out[0].length, 0.4);
    }

    #[test]
    fn quantize_half_strength_moves_halfway() {
        let out = quantize(&[note(0.1, 0.5, 60, 90)], 0.25, 0.5, false);
        assert!((out[0].start - 0.05).abs() < 1e-12);
    }

    #[test]
    fn quantize_lengths_use_whole_steps_and_at_least_one() {
        let out = quantize(
            &[note(0.0, 0.3, 60, 90), note(1.0, 0.01, 60, 90)],
            0.25,
            1.0,
            true,
        );
        assert_eq!(out[0].length, 0.25);
        assert_eq!(out[1].length, 0.25);
    }

    #[test]
    fn quantize_ignores_a_bad_grid() {
        let notes = [note(0.1, 0.5, 60, 90)];
        assert_eq!(quantize(&notes, 0.0, 1.0, true), notes);
        assert_eq!(quantize(&notes, f64::NAN, 1.0, true), notes);
    }

    #[test]
    fn legato_any_key_joins_to_the_next_onset() {
        let notes = [
            note(0.0, 0.25, 60, 90),
            note(1.0, 0.25, 64, 90),
            note(1.0, 0.25, 67, 90),
            note(2.5, 0.25, 60, 90),
        ];
        let out = legato(&notes, LegatoMode::AnyKey);
        assert_eq!(out[0].length, 1.0);
        assert_eq!(out[1].length, 1.5);
        assert_eq!(out[2].length, 1.5);
        assert_eq!(out[3].length, 0.25);
    }

    #[test]
    fn legato_same_key_ignores_other_keys() {
        let notes = [
            note(0.0, 0.25, 60, 90),
            note(1.0, 0.25, 64, 90),
            note(2.0, 0.25, 60, 90),
        ];
        let out = legato(&notes, LegatoMode::SameKey);
        assert_eq!(out[0].length, 2.0);
        assert_eq!(out[1].length, 0.25);
    }

    #[test]
    fn legato_trims_overlaps() {
        let notes = [note(0.0, 3.0, 60, 90), note(1.0, 1.0, 62, 90)];
        assert_eq!(legato(&notes, LegatoMode::AnyKey)[0].length, 1.0);
    }

    #[test]
    fn humanize_is_deterministic_and_bounded() {
        let notes: Vec<Note> = (0..32)
            .map(|i| note(f64::from(i) * 0.5, 0.25, 60, 100))
            .collect();
        let a = humanize(&notes, 7, 0.02, 8);
        assert_eq!(a, humanize(&notes, 7, 0.02, 8));
        assert_ne!(a, humanize(&notes, 8, 0.02, 8));
        for (new, old) in a.iter().zip(&notes) {
            assert!((new.start - old.start).abs() <= 0.02 + 1e-12);
            assert!(new.velocity.abs_diff(old.velocity) <= 8);
            assert_eq!(new.key, old.key);
            assert_eq!(new.length, old.length);
        }
        assert!(a.iter().any(|n| n.velocity != 100), "something moved");
    }

    #[test]
    fn humanize_keeps_starts_and_velocities_in_range() {
        let notes = [note(0.0, 0.25, 60, 1), note(0.001, 0.25, 60, 127)];
        for seed in 0..50 {
            for n in humanize(&notes, seed, 0.5, 100) {
                assert!(n.start >= 0.0);
                assert!((1..=127).contains(&n.velocity));
            }
        }
    }

    #[test]
    fn transpose_moves_and_drops_out_of_range() {
        let notes = [note(0.0, 1.0, 60, 90), note(1.0, 1.0, 126, 90)];
        assert_eq!(transpose(&notes, 12).len(), 1);
        assert_eq!(transpose(&notes, 12)[0].key, 72);
        assert_eq!(transpose(&notes, -61).len(), 1);
        assert_eq!(transpose(&notes, -61)[0].key, 65);
    }

    #[test]
    fn velocity_scale_and_set_clamp() {
        let notes = [note(0.0, 1.0, 60, 100), note(1.0, 1.0, 60, 1)];
        let scaled = scale_velocity(&notes, 0.5);
        assert_eq!(scaled[0].velocity, 50);
        assert_eq!(scaled[1].velocity, 1);
        assert_eq!(scale_velocity(&notes, 3.0)[0].velocity, 127);
        assert!(set_velocity(&notes, 0).iter().all(|n| n.velocity == 1));
        assert!(set_velocity(&notes, 90).iter().all(|n| n.velocity == 90));
    }

    #[test]
    fn selection_restricts_the_edit_and_survives_removed_notes() {
        let notes = [
            note(0.0, 1.0, 60, 90),
            note(1.0, 1.0, 127, 90),
            note(2.0, 1.0, 62, 90),
        ];
        let up = MidiEdit::Transpose { semitones: 1 };
        // Notes 1 and 2 selected: note 1 leaves the range and is removed; note 0 is untouched.
        let out = up.apply(&notes, Some(&[1, 2, 2, 9]));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].key, 60);
        assert_eq!(out[1].key, 63);
        let all = up.apply(&notes, None);
        assert_eq!(all.iter().map(|n| n.key).collect::<Vec<_>>(), [61, 63]);
    }

    #[test]
    fn edits_parse_from_json_with_defaults() {
        let edit: MidiEdit = serde_json::from_str(r#"{"op":"quantize","grid":0.25}"#).unwrap();
        assert_eq!(
            edit,
            MidiEdit::Quantize {
                grid: 0.25,
                strength: 1.0,
                lengths: false
            }
        );
        let edit: MidiEdit = serde_json::from_str(r#"{"op":"humanize","seed":3}"#).unwrap();
        assert!(matches!(
            edit,
            MidiEdit::Humanize {
                seed: 3,
                velocity: 8,
                ..
            }
        ));
    }
}
