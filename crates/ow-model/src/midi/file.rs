//! Standard MIDI Files (format 0 and 1) through `midly`, on byte slices.
//!
//! [`import`] reads a file into one [`ImportedTrack`] per track that holds notes or control
//! changes (format 0 is one track; format 1's tempo-only conductor track is skipped), with
//! positions in beats (one beat is one quarter note: ticks divided by the file's pulses per
//! quarter note). The file's tempo changes and first time signature are returned for the caller
//! to use or ignore. [`export`] writes a format 1 file at 960 pulses per quarter note: a
//! conductor track with the tempo points and time signature, then one track per [`ExportTrack`].
//! A tempo ramp is written as a step at its point.

use std::fmt;

use midly::num::{u4, u7, u15, u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

use super::MIN_NOTE_LENGTH;
use crate::model::{Controller, Note};
use crate::time::{TempoMap, TimeSignature};

/// Pulses per quarter note of exported files.
pub const EXPORT_PPQ: u16 = 960;

/// Why a MIDI file could not be read or written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiFileError {
    /// The bytes are not a valid Standard MIDI File.
    Parse(String),
    /// A valid file this build does not read (SMPTE timing, format 2).
    Unsupported(String),
    /// The file could not be written.
    Write(String),
}

impl fmt::Display for MidiFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "not a valid MIDI file: {e}"),
            Self::Unsupported(e) => write!(f, "unsupported MIDI file: {e}"),
            Self::Write(e) => write!(f, "could not write the MIDI file: {e}"),
        }
    }
}

impl std::error::Error for MidiFileError {}

/// One track of an imported file, positions in beats from the start of the file.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedTrack {
    /// The track's name event, or empty.
    pub name: String,
    /// Notes, ordered by start.
    pub notes: Vec<Note>,
    /// Control changes, ordered by position.
    pub controllers: Vec<Controller>,
    /// Beat of the track's last event (including its end-of-track marker).
    pub end: f64,
}

impl ImportedTrack {
    /// The beat the content starts at, rounded down to a whole beat: the natural clip start.
    pub fn start(&self) -> f64 {
        let first = self
            .notes
            .iter()
            .map(|n| n.start)
            .chain(self.controllers.iter().map(|c| c.beat))
            .fold(f64::INFINITY, f64::min);
        if first.is_finite() {
            first.floor()
        } else {
            0.0
        }
    }

    /// Length from [`start`](Self::start) to the end of the last note or event, rounded up to a
    /// whole beat (at least one).
    pub fn length(&self) -> f64 {
        let last = self
            .notes
            .iter()
            .map(|n| n.start + n.length)
            .chain(self.controllers.iter().map(|c| c.beat))
            .fold(0.0_f64, f64::max);
        (last - self.start()).ceil().max(1.0)
    }
}

/// A file read by [`import`].
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedMidi {
    /// Tracks that hold notes or control changes.
    pub tracks: Vec<ImportedTrack>,
    /// Tempo changes as `(beat, bpm)`, in order; empty when the file has none.
    pub tempo: Vec<(f64, f64)>,
    /// The first time signature as `(numerator, denominator)`, if the file has one.
    pub time_signature: Option<(u8, u32)>,
}

/// Reads a Standard MIDI File (format 0 or 1, pulses-per-quarter timing).
pub fn import(bytes: &[u8]) -> Result<ImportedMidi, MidiFileError> {
    let smf = Smf::parse(bytes).map_err(|e| MidiFileError::Parse(e.to_string()))?;
    let ppq = match smf.header.timing {
        Timing::Metrical(ppq) if ppq.as_int() > 0 => f64::from(ppq.as_int()),
        Timing::Metrical(_) => return Err(MidiFileError::Parse("zero pulses per quarter".into())),
        Timing::Timecode(..) => {
            return Err(MidiFileError::Unsupported(
                "SMPTE timecode timing is not supported".into(),
            ));
        }
    };
    if smf.header.format == Format::Sequential {
        return Err(MidiFileError::Unsupported(
            "format 2 (sequential patterns) is not supported".into(),
        ));
    }
    let mut tracks = Vec::new();
    let mut tempo: Vec<(f64, f64)> = Vec::new();
    let mut time_signature = None;
    for events in &smf.tracks {
        let mut name = String::new();
        let mut notes: Vec<Note> = Vec::new();
        let mut controllers = Vec::new();
        // Sounding notes by (channel, key): start tick and velocity, oldest first.
        let mut sounding: Vec<Vec<(u64, u8)>> = vec![Vec::new(); 16 * 128];
        let mut tick = 0_u64;
        for event in events {
            tick += u64::from(event.delta.as_int());
            let beat = tick as f64 / ppq;
            match event.kind {
                TrackEventKind::Meta(MetaMessage::TrackName(raw)) if name.is_empty() => {
                    name = String::from_utf8_lossy(raw).trim().to_string();
                }
                TrackEventKind::Meta(MetaMessage::Tempo(us)) if us.as_int() > 0 => {
                    let bpm = 60_000_000.0 / f64::from(us.as_int());
                    tempo.push((beat, bpm));
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(num, den_pow, ..))
                    if time_signature.is_none() && num > 0 && den_pow <= 6 =>
                {
                    time_signature = Some((num, 1_u32 << den_pow));
                }
                TrackEventKind::Midi { channel, message } => {
                    let slot =
                        |key: u7| usize::from(channel.as_int()) * 128 + usize::from(key.as_int());
                    match message {
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            sounding[slot(key)].push((tick, vel.as_int()));
                        }
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                            let held = &mut sounding[slot(key)];
                            if !held.is_empty() {
                                let (start, velocity) = held.remove(0);
                                notes.push(note_between(start, tick, key.as_int(), velocity, ppq));
                            }
                        }
                        MidiMessage::Controller { controller, value } => {
                            controllers.push(Controller {
                                beat,
                                controller: controller.as_int(),
                                value: value.as_int(),
                            });
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        // Notes without a note-off end with the track.
        for (index, held) in sounding.into_iter().enumerate() {
            for (start, velocity) in held {
                let key = (index % 128) as u8;
                notes.push(note_between(start, tick.max(start), key, velocity, ppq));
            }
        }
        if notes.is_empty() && controllers.is_empty() {
            continue;
        }
        notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.key.cmp(&b.key)));
        tracks.push(ImportedTrack {
            name,
            notes,
            controllers,
            end: tick as f64 / ppq,
        });
    }
    // Tempo events may sit in any track: order them by position (a later event at the same
    // beat wins) and drop the ones that change nothing.
    tempo.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut ordered: Vec<(f64, f64)> = Vec::with_capacity(tempo.len());
    for (beat, bpm) in tempo {
        match ordered.last_mut() {
            Some(last) if last.0 == beat => last.1 = bpm,
            _ => ordered.push((beat, bpm)),
        }
    }
    let mut tempo: Vec<(f64, f64)> = Vec::with_capacity(ordered.len());
    for (beat, bpm) in ordered {
        if tempo
            .last()
            .is_none_or(|(_, last)| (last - bpm).abs() > 1e-9)
        {
            tempo.push((beat, bpm));
        }
    }
    Ok(ImportedMidi {
        tracks,
        tempo,
        time_signature,
    })
}

fn note_between(start: u64, end: u64, key: u8, velocity: u8, ppq: f64) -> Note {
    Note {
        start: start as f64 / ppq,
        length: ((end - start) as f64 / ppq).max(MIN_NOTE_LENGTH),
        key,
        velocity,
    }
}

/// One track to write: notes and controllers at absolute beats (the caller adds clip starts).
#[derive(Debug, Clone, Copy)]
pub struct ExportTrack<'a> {
    /// Written as the track name.
    pub name: &'a str,
    /// Notes.
    pub notes: &'a [Note],
    /// Control changes.
    pub controllers: &'a [Controller],
}

/// Writes `tracks` as a format 1 file at [`EXPORT_PPQ`] pulses per quarter note, all on MIDI
/// channel 1, with the tempo points of `tempo` and `signature` in a conductor track.
pub fn export(
    tracks: &[ExportTrack<'_>],
    tempo: &TempoMap,
    signature: TimeSignature,
) -> Result<Vec<u8>, MidiFileError> {
    let ppq = f64::from(EXPORT_PPQ);
    let ticks = |beat: f64| (beat.max(0.0) * ppq).round() as u64;
    let mut smf = Smf::new(Header::new(
        Format::Parallel,
        Timing::Metrical(u15::new(EXPORT_PPQ)),
    ));

    // Each entry: absolute tick, order within a tick, kind. Offs sort before ons and the like.
    type Pending<'a> = Vec<(u64, u8, TrackEventKind<'a>)>;
    let mut conductor: Pending<'_> = vec![(
        0,
        0,
        TrackEventKind::Meta(MetaMessage::TimeSignature(
            signature.numerator().min(255) as u8,
            signature.denominator().trailing_zeros() as u8,
            24,
            8,
        )),
    )];
    for point in tempo.points() {
        let micros = (60_000_000.0 / point.bpm).round().clamp(1.0, 16_777_215.0) as u32;
        conductor.push((
            ticks(point.beat),
            1,
            TrackEventKind::Meta(MetaMessage::Tempo(u24::new(micros))),
        ));
    }
    smf.tracks.push(finish_track(conductor)?);

    let channel = u4::new(0);
    for track in tracks {
        let mut pending: Pending<'_> = vec![(
            0,
            0,
            TrackEventKind::Meta(MetaMessage::TrackName(track.name.as_bytes())),
        )];
        for note in track.notes {
            let key = u7::new(note.key.min(127));
            let on = ticks(note.start);
            let off = (on + 1).max(ticks(note.start + note.length));
            pending.push((
                on,
                3,
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOn {
                        key,
                        vel: u7::new(note.velocity.clamp(1, 127)),
                    },
                },
            ));
            pending.push((
                off,
                1,
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOff {
                        key,
                        vel: u7::new(0),
                    },
                },
            ));
        }
        for cc in track.controllers {
            pending.push((
                ticks(cc.beat),
                2,
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::Controller {
                        controller: u7::new(cc.controller.min(127)),
                        value: u7::new(cc.value.min(127)),
                    },
                },
            ));
        }
        smf.tracks.push(finish_track(pending)?);
    }
    let mut bytes = Vec::new();
    smf.write_std(&mut bytes)
        .map_err(|e| MidiFileError::Write(e.to_string()))?;
    Ok(bytes)
}

/// Orders `pending` by tick (stable within equal ticks and order), turns ticks into deltas and
/// ends the track.
fn finish_track(
    mut pending: Vec<(u64, u8, TrackEventKind<'_>)>,
) -> Result<Vec<TrackEvent<'_>>, MidiFileError> {
    pending.sort_by_key(|(tick, order, _)| (*tick, *order));
    let mut out = Vec::with_capacity(pending.len() + 1);
    let mut previous = 0_u64;
    for (tick, _, kind) in pending {
        let delta = u32::try_from(tick - previous)
            .ok()
            .filter(|d| *d <= u28::max_value().as_int())
            .ok_or_else(|| MidiFileError::Write("a position is too far from the start".into()))?;
        out.push(TrackEvent {
            delta: u28::new(delta),
            kind,
        });
        previous = tick;
    }
    out.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
    });
    Ok(out)
}

#[cfg(test)]
mod midi_file_tests {
    use super::*;
    use crate::time::TempoPoint;

    fn note(start: f64, length: f64, key: u8, velocity: u8) -> Note {
        Note {
            start,
            length,
            key,
            velocity,
        }
    }

    #[test]
    fn export_then_import_round_trips_notes_and_controllers() {
        let melody = [
            note(0.0, 1.0, 60, 100),
            note(1.0, 0.5, 64, 80),
            note(1.5, 0.25, 67, 127),
            note(4.0, 2.0, 72, 1),
        ];
        let pedal = [Controller {
            beat: 2.0,
            controller: 64,
            value: 127,
        }];
        let bass = [note(0.0, 4.0, 36, 90)];
        let tempo = TempoMap::new(vec![
            TempoPoint::constant(0.0, 100.0),
            TempoPoint::constant(8.0, 140.0),
        ])
        .unwrap();
        let bytes = export(
            &[
                ExportTrack {
                    name: "Melody",
                    notes: &melody,
                    controllers: &pedal,
                },
                ExportTrack {
                    name: "Bass",
                    notes: &bass,
                    controllers: &[],
                },
            ],
            &tempo,
            TimeSignature::new(3, 4).unwrap(),
        )
        .unwrap();
        let file = import(&bytes).unwrap();
        assert_eq!(file.tracks.len(), 2, "the conductor track is skipped");
        assert_eq!(file.tracks[0].name, "Melody");
        assert_eq!(file.tracks[0].notes, melody);
        assert_eq!(file.tracks[0].controllers, pedal);
        assert_eq!(file.tracks[1].name, "Bass");
        assert_eq!(file.tracks[1].notes, bass);
        // A tempo is stored as whole microseconds per quarter note.
        assert_eq!(file.tempo.len(), 2);
        assert!(
            file.tempo
                .iter()
                .zip([(0.0, 100.0), (8.0, 140.0)])
                .all(|((beat, bpm), (want_beat, want_bpm))| *beat == want_beat
                    && (bpm - want_bpm).abs() < 0.01)
        );
        assert_eq!(file.time_signature, Some((3, 4)));
    }

    #[test]
    fn round_trip_keeps_notes_off_the_tick_grid_within_a_tick() {
        let notes = [note(0.123_456, 0.333_333, 61, 99)];
        let bytes = export(
            &[ExportTrack {
                name: "t",
                notes: &notes,
                controllers: &[],
            }],
            &TempoMap::default(),
            TimeSignature::COMMON,
        )
        .unwrap();
        let back = &import(&bytes).unwrap().tracks[0].notes[0];
        let tick = 1.0 / f64::from(EXPORT_PPQ);
        assert!((back.start - notes[0].start).abs() <= tick);
        assert!((back.length - notes[0].length).abs() <= 2.0 * tick);
    }

    #[test]
    fn track_start_and_length_are_whole_beats() {
        let notes = [note(4.25, 1.0, 60, 90), note(6.0, 0.5, 62, 90)];
        let bytes = export(
            &[ExportTrack {
                name: "t",
                notes: &notes,
                controllers: &[],
            }],
            &TempoMap::default(),
            TimeSignature::COMMON,
        )
        .unwrap();
        let track = &import(&bytes).unwrap().tracks[0];
        assert_eq!(track.start(), 4.0);
        assert_eq!(track.length(), 3.0);
    }

    #[test]
    fn imports_format_0_with_running_status_and_unclosed_notes() {
        // Header: format 0, one track, 96 ticks per quarter. Track: note on, note on (running
        // status), note off for the first, end of track; the second note never ends explicitly.
        let mut bytes = b"MThd\0\0\0\x06\0\0\0\x01\0\x60MTrk".to_vec();
        let body: Vec<u8> = vec![
            0x00, 0x90, 60, 100, // on 60
            0x00, 62, 90, // on 62 (running status)
            0x60, 0x80, 60, 0, // off 60 after 96 ticks
            0x60, 0xFF, 0x2F, 0x00, // end of track after another 96
        ];
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&body);
        let file = import(&bytes).unwrap();
        let notes = &file.tracks[0].notes;
        assert_eq!(notes.len(), 2);
        assert_eq!(
            (notes[0].key, notes[0].start, notes[0].length),
            (60, 0.0, 1.0)
        );
        assert_eq!((notes[1].key, notes[1].length), (62, 2.0));
    }

    #[test]
    fn tempo_events_from_several_tracks_are_ordered_by_position() {
        let tempo_at = |delta: u32, micros: u32| TrackEvent {
            delta: u28::new(delta),
            kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(micros))),
        };
        let end = || TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        };
        let note = |on: bool| TrackEvent {
            delta: u28::new(if on { 0 } else { 96 }),
            kind: TrackEventKind::Midi {
                channel: u4::new(0),
                message: if on {
                    MidiMessage::NoteOn {
                        key: u7::new(60),
                        vel: u7::new(90),
                    }
                } else {
                    MidiMessage::NoteOff {
                        key: u7::new(60),
                        vel: u7::new(0),
                    }
                },
            },
        };
        let mut smf = Smf::new(Header::new(
            Format::Parallel,
            Timing::Metrical(u15::new(96)),
        ));
        // The first track changes the tempo at beat 4, the second at beat 2 and at beat 0.
        smf.tracks
            .push(vec![tempo_at(384, 400_000), note(true), note(false), end()]);
        smf.tracks
            .push(vec![tempo_at(0, 500_000), tempo_at(192, 600_000), end()]);
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).unwrap();
        let tempo = import(&bytes).unwrap().tempo;
        assert_eq!(tempo.len(), 3);
        assert_eq!(
            tempo.iter().map(|t| t.0).collect::<Vec<_>>(),
            [0.0, 2.0, 4.0]
        );
        assert!((tempo[0].1 - 120.0).abs() < 1e-9 && (tempo[2].1 - 150.0).abs() < 1e-9);
    }

    #[test]
    fn rejects_garbage_and_smpte_timing() {
        assert!(matches!(import(b"not midi"), Err(MidiFileError::Parse(_))));
        // SMPTE: the division's top bit set (-25 fps, 40 ticks per frame).
        let mut bytes = b"MThd\0\0\0\x06\0\0\0\x01\xE7\x28MTrk\0\0\0\x04\x00\xFF\x2F\x00".to_vec();
        assert!(matches!(import(&bytes), Err(MidiFileError::Unsupported(_))));
        bytes[9] = 2; // format 2
        bytes[12] = 0;
        bytes[13] = 0x60;
        assert!(import(&bytes).is_err());
    }
}
