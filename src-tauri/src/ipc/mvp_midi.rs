//! MIDI recording, editing and files through the app's control layer, without a window: MIDI
//! played into the mock MIDI port while the engine records becomes clips by the three record
//! modes in one undo step, and `midi_edit`, `midi_export` and `midi_import` change and read the
//! project through the same plain methods the Tauri commands and MCP tools wrap.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ow_engine::{Engine, EngineConfig, MidiClock, MidiInputSelection};
use ow_io::mock::{MockBackend, MockConfig, MockDrive, MockHandle, MockMidiBackend};
use ow_io::{AudioBackend, AudioSelection, BackendRegistry, MidiBackend, StreamRequest};
use ow_ipc::{EditCommand, UiCommand};
use ow_model::midi::{MidiEdit, MidiRecordMode};
use ow_model::{
    BeatRange, Clip, ClipContent, Device, DeviceId, Note, Owned, ProjectSettings, Strip, Track,
    TrackId, TrackKind,
};

use super::mvp::{CompilingTarget, command, edit};
use super::{IpcState, lock};

const RATE: u32 = 48_000;
/// One beat at the default 120 BPM.
const BEAT: u64 = 24_000;
const PORT: &str = "keys";

struct Rig {
    engine: Engine,
    audio: MockHandle,
    midi: Arc<MockMidiBackend>,
    blocks: u64,
}

fn rig() -> Rig {
    let backend = MockBackend::new(MockConfig {
        drive: MockDrive::Manual,
        buffer_frames: 64,
        ..MockConfig::default()
    });
    let audio = backend.handle();
    let registry = BackendRegistry::with(vec![Arc::new(backend) as Arc<dyn AudioBackend>]);
    let midi = Arc::new(MockMidiBackend::new([PORT]));
    let config = EngineConfig {
        audio: AudioSelection {
            backend: "mock".into(),
            request: StreamRequest::default(),
        },
        midi_inputs: vec![MidiInputSelection {
            port: PORT.into(),
            control_surface: false,
        }],
        midi_clock: MidiClock::StreamFrames,
        ..EngineConfig::default()
    };
    let engine = Engine::with_backends(registry, Arc::clone(&midi) as Arc<dyn MidiBackend>, config);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(matches!(engine.audio_status().state, ow_io::AudioState::Running)
        && engine.midi().status().ports.iter().any(|p| p.connected))
    {
        assert!(Instant::now() < deadline, "the engine did not start");
        std::thread::sleep(Duration::from_millis(2));
    }
    Rig {
        engine,
        audio,
        midi,
        blocks: 0,
    }
}

impl Rig {
    /// Plays `messages` (stream frame, bytes), each sent one block before its frame, for
    /// `blocks` more blocks.
    fn play(&mut self, messages: &[(u64, [u8; 3])], blocks: u64) {
        for _ in 0..blocks {
            let mut sent = false;
            for (frame, bytes) in messages {
                if frame / 64 == self.blocks + 1 {
                    let stamp = (*frame as f64 * 1e6 / f64::from(RATE)).round() as u64;
                    assert!(self.midi.send(PORT, stamp, bytes));
                    sent = true;
                }
            }
            if sent {
                self.midi.flush(PORT);
            }
            self.audio.run_callbacks(1).unwrap();
            self.blocks += 1;
        }
    }

    /// Stops through the app while a helper thread keeps the manual device running.
    fn stop(&self, state: &IpcState) -> Result<(), String> {
        let running = Arc::new(AtomicBool::new(true));
        let pump = {
            let running = Arc::clone(&running);
            let audio = self.audio.clone();
            std::thread::spawn(move || {
                while running.load(Ordering::Relaxed) {
                    audio.run_callbacks(1).unwrap();
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        };
        let result = state.transport_stop(&self.engine);
        running.store(false, Ordering::Relaxed);
        pump.join().unwrap();
        result
    }
}

fn add_midi_track(state: &IpcState, engine: &Engine, clips: Vec<Clip>) -> TrackId {
    let ids = command(state, engine, UiCommand::AllocIds { count: 2 });
    let (track, synth) = (TrackId(ids[0]), DeviceId(ids[1]));
    let index = lock(&state.control).session().project().tracks.len();
    let clips = clips
        .into_iter()
        .map(|mut clip| {
            let id = command(state, engine, UiCommand::AllocIds { count: 1 })[0];
            clip.id = id.into();
            clip.track = track;
            clip
        })
        .collect();
    edit(
        state,
        engine,
        EditCommand::InsertTrack {
            index,
            bundle: Box::new(Owned {
                item: Track {
                    id: track,
                    name: "Keys".into(),
                    color: 0,
                    kind: TrackKind::Midi,
                    strip: Strip::default(),
                    instrument: Some(synth),
                    input: None,
                    monitor: Default::default(),
                },
                clips,
                devices: vec![Device {
                    id: synth,
                    kind: "builtin.synth".into(),
                    name: "Synth".into(),
                    enabled: true,
                    params: Default::default(),
                    state: vec![],
                    sidechain: None,
                }],
                lanes: vec![],
            }),
        },
    );
    track
}

fn note(start: f64, length: f64, key: u8) -> Note {
    Note {
        start,
        length,
        key,
        velocity: 100,
    }
}

fn midi_clip(start: f64, length: f64, notes: Vec<Note>) -> Clip {
    Clip {
        id: 0.into(),
        track: 0.into(),
        name: "clip".into(),
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

fn clips_on(state: &IpcState, track: TrackId) -> Vec<Clip> {
    let mut clips: Vec<Clip> = lock(&state.control)
        .session()
        .project()
        .clips
        .values()
        .filter(|c| c.track == track)
        .cloned()
        .collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    clips
}

fn notes_of(clip: &Clip) -> Vec<Note> {
    match &clip.content {
        ClipContent::Midi { notes, .. } => notes.clone(),
        _ => panic!("a MIDI clip"),
    }
}

/// A note played on `key` from `from` for `frames` frames, as messages.
fn played(from: u64, frames: u64, key: u8) -> [(u64, [u8; 3]); 2] {
    [(from, [0x90, key, 100]), (from + frames, [0x80, key, 0])]
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1.0 / 960.0
}

#[test]
fn midi_record_replace_adds_a_clip_with_the_played_notes_as_one_undo_step() {
    let mut rig = rig();
    let state = IpcState::new();
    let track = add_midi_track(&state, &rig.engine, vec![]);
    assert!(
        state.record_set_arm(track.0, true).is_ok(),
        "a MIDI track can be armed"
    );
    state.record_start(&rig.engine).unwrap();
    // C4 from beat 1 for half a beat; E4 from beat 2.5 for one beat.
    let mut messages = played(BEAT, BEAT / 2, 60).to_vec();
    messages.extend(played(5 * BEAT / 2, BEAT, 64));
    rig.play(&messages, 4 * BEAT / 64);
    rig.stop(&state).unwrap();

    let clips = clips_on(&state, track);
    assert_eq!(clips.len(), 1, "one new clip: {clips:?}");
    assert!(close(clips[0].start, 0.0), "starts with the recording");
    assert!(clips[0].length > 3.9, "length {}", clips[0].length);
    let notes = notes_of(&clips[0]);
    assert_eq!(notes.len(), 2);
    assert_eq!((notes[0].key, notes[1].key), (60, 64));
    assert!(close(notes[0].start, 1.0) && close(notes[0].length, 0.5));
    assert!(close(notes[1].start, 2.5) && close(notes[1].length, 1.0));
    assert_eq!(notes[0].velocity, 100);

    // The whole recording is one undo step.
    let undo_label = lock(&state.control).snapshot().history.undo_label.clone();
    assert_eq!(undo_label.as_deref(), Some("Record"));
    command(&state, &rig.engine, UiCommand::Undo);
    assert!(clips_on(&state, track).is_empty());
}

#[test]
fn midi_record_overdub_joins_the_existing_clip_and_replace_cuts_it() {
    for (mode, expect_merge) in [
        (MidiRecordMode::Overdub, true),
        (MidiRecordMode::Replace, false),
    ] {
        let mut rig = rig();
        let state = IpcState::new();
        // An existing clip over beats 0 to 8 with a note at beat 6, outside the recording.
        let track = add_midi_track(
            &state,
            &rig.engine,
            vec![midi_clip(
                0.0,
                8.0,
                vec![note(0.0, 1.0, 50), note(6.0, 1.0, 52)],
            )],
        );
        let original = clips_on(&state, track)[0].id;
        state.record_set_arm(track.0, true).unwrap();
        state.record_start_with(&rig.engine, Some(mode)).unwrap();
        rig.play(&played(BEAT, BEAT / 2, 60), 2 * BEAT / 64);
        rig.stop(&state).unwrap();

        let clips = clips_on(&state, track);
        if expect_merge {
            assert_eq!(clips.len(), 1, "{clips:?}");
            assert_eq!(clips[0].id, original);
            assert_eq!((clips[0].start, clips[0].length), (0.0, 8.0));
            let keys: Vec<u8> = notes_of(&clips[0]).iter().map(|n| n.key).collect();
            assert_eq!(keys, [50, 60, 52], "merged in time order");
        } else {
            // Recorded range is beat 0 to just under 2: the old clip keeps what is after it.
            assert_eq!(clips.len(), 2, "{clips:?}");
            assert!(close(clips[0].start, 0.0));
            assert_eq!(
                notes_of(&clips[0])
                    .iter()
                    .map(|n| n.key)
                    .collect::<Vec<_>>(),
                [60]
            );
            assert!(clips[1].start > 1.9 && clips[1].start < 2.1);
            assert_eq!(
                notes_of(&clips[1])
                    .iter()
                    .map(|n| n.key)
                    .collect::<Vec<_>>(),
                [52]
            );
            assert_eq!(clips[1].id, original, "the surviving piece keeps its id");
        }
    }
}

#[test]
fn midi_record_loop_merge_makes_one_clip_from_every_lap_and_replace_keeps_each_lap() {
    for mode in [MidiRecordMode::LoopMerge, MidiRecordMode::Replace] {
        let mut rig = rig();
        let state = IpcState::new();
        let track = add_midi_track(&state, &rig.engine, vec![]);
        let mut settings: ProjectSettings =
            lock(&state.control).session().project().settings.clone();
        settings.loop_range = Some(BeatRange {
            start: 0.0,
            end: 2.0,
        });
        edit(&state, &rig.engine, EditCommand::SetSettings { settings });
        state.record_set_arm(track.0, true).unwrap();
        state.record_start_with(&rig.engine, Some(mode)).unwrap();
        // Lap one (frames 0 to 48000) plays C at beat 0.5, lap two plays E at beat 1.5 of the
        // loop (stream frame 2 beats + 1.5 beats).
        let mut messages = played(BEAT / 2, BEAT / 4, 60).to_vec();
        messages.extend(played(2 * BEAT + 3 * BEAT / 2, BEAT / 4, 64));
        rig.play(&messages, 2 * 2 * BEAT / 64 - 8);
        rig.stop(&state).unwrap();

        let clips = clips_on(&state, track);
        match mode {
            MidiRecordMode::LoopMerge => {
                assert_eq!(clips.len(), 1, "{clips:?}");
                assert!(clips[0].take.is_none());
                let notes = notes_of(&clips[0]);
                assert_eq!(notes.len(), 2);
                assert!(close(notes[0].start, 0.5) && close(notes[1].start, 1.5));
                assert_eq!((notes[0].key, notes[1].key), (60, 64));
            }
            _ => {
                assert_eq!(clips.len(), 2, "one clip per lap: {clips:?}");
                assert!(clips.iter().all(|c| c.take.is_none()));
                assert_eq!((clips[0].muted, clips[1].muted), (true, false));
                assert_eq!(notes_of(&clips[0])[0].key, 60);
                assert_eq!(notes_of(&clips[1])[0].key, 64);
            }
        }
    }
}

#[test]
fn midi_edit_quantizes_one_clip_in_one_undo_step() {
    let state = IpcState::new();
    let target = CompilingTarget::default();
    let track = add_midi_track_compiled(&state, &target);
    let clip = clips_on(&state, track)[0].id;
    let reply = state
        .midi_edit(
            clip.0,
            &MidiEdit::Quantize {
                grid: 0.25,
                strength: 1.0,
                lengths: false,
            },
            Some(&[0]),
            &target,
        )
        .unwrap();
    assert_eq!((reply.notes_before, reply.notes_after), (2, 2));
    let notes = notes_of(&clips_on(&state, track)[0]);
    assert_eq!(notes[0].start, 0.25, "the selected note moved");
    assert_eq!(notes[1].start, 1.1, "the other did not");
    assert_eq!(
        lock(&state.control)
            .snapshot()
            .history
            .undo_label
            .as_deref(),
        Some("Quantize")
    );
    command(&state, &target, UiCommand::Undo);
    assert_eq!(notes_of(&clips_on(&state, track)[0])[0].start, 0.2);
    assert!(
        state
            .midi_edit(9_999, &MidiEdit::Transpose { semitones: 1 }, None, &target)
            .is_err()
    );
}

/// A MIDI track with one clip of two notes (at beats 0.2 and 1.1), for tests that compile the
/// project instead of running the engine.
fn add_midi_track_compiled(state: &IpcState, target: &CompilingTarget) -> TrackId {
    let ids = command(state, target, UiCommand::AllocIds { count: 3 });
    let (track, synth) = (TrackId(ids[0]), DeviceId(ids[1]));
    let mut clip = midi_clip(0.0, 4.0, vec![note(0.2, 0.5, 60), note(1.1, 0.5, 64)]);
    clip.id = ids[2].into();
    clip.track = track;
    edit(
        state,
        target,
        EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned {
                item: Track {
                    id: track,
                    name: "Keys".into(),
                    color: 0,
                    kind: TrackKind::Midi,
                    strip: Strip::default(),
                    instrument: Some(synth),
                    input: None,
                    monitor: Default::default(),
                },
                clips: vec![clip],
                devices: vec![Device {
                    id: synth,
                    kind: "builtin.synth".into(),
                    name: "Synth".into(),
                    enabled: true,
                    params: Default::default(),
                    state: vec![],
                    sidechain: None,
                }],
                lanes: vec![],
            }),
        },
    );
    track
}

#[test]
fn a_failed_import_leaves_nothing_behind_and_a_gesture_is_not_cancelled() {
    let state = IpcState::new();
    let target = CompilingTarget::default();
    let track = |name: &str, key: u8| ow_model::midi::ImportedTrack {
        name: name.into(),
        notes: vec![Note {
            key,
            ..note(0.0, 1.0, 60)
        }],
        controllers: vec![],
        end: 1.0,
    };
    // The first file track imports fine; the second has a key the model refuses.
    let file = ow_model::midi::ImportedMidi {
        tracks: vec![track("good", 60), track("bad", 200)],
        tempo: vec![],
        time_signature: None,
    };
    let before = lock(&state.control).snapshot().project;
    let tempo = ow_model::time::TempoMap::constant(90.0).unwrap();
    let result = super::midi::import_atomically(
        &mut lock(&state.control),
        &file,
        None,
        0.0,
        "x",
        Some(tempo.clone()),
        &target,
    );
    assert!(result.is_err());
    let after = lock(&state.control).snapshot();
    // Reserved ids are never given back; everything else is as it was.
    let mut restored = after.project.clone();
    restored.next_id = before.next_id;
    assert_eq!(
        restored, before,
        "the first track and the tempo change were rolled back"
    );
    assert!(!after.history.in_transaction);
    assert!(!after.history.can_undo);

    // Inside someone else's open edit the import is refused and that edit survives.
    command(
        &state,
        &target,
        UiCommand::Begin {
            label: "Drag".into(),
        },
    );
    edit(
        &state,
        &target,
        EditCommand::SetName {
            name: "mid-gesture".into(),
        },
    );
    let refused = super::midi::import_atomically(
        &mut lock(&state.control),
        &file,
        None,
        0.0,
        "x",
        None,
        &target,
    )
    .unwrap_err();
    assert!(refused.contains("open edit"), "{refused}");
    let snapshot = lock(&state.control).snapshot();
    assert!(snapshot.history.in_transaction);
    assert_eq!(snapshot.project.name, "mid-gesture");
}

#[test]
fn a_muted_lap_is_not_exported_unless_named() {
    let state = IpcState::new();
    let target = CompilingTarget::default();
    let track = add_midi_track_compiled(&state, &target);
    let mut laps = Vec::new();
    for key in [60_u8, 62] {
        let mut clip = midi_clip(4.0, 4.0, vec![note(0.0, 1.0, key)]);
        clip.id = command(&state, &target, UiCommand::AllocIds { count: 1 })[0].into();
        clip.track = track;
        clip.muted = key == 60;
        laps.push(clip.clone());
        edit(
            &state,
            &target,
            EditCommand::AddClip {
                clip: Box::new(clip),
            },
        );
    }
    let work = tempfile::tempdir().unwrap();
    let all = work.path().join("all.mid");
    // The track's own clip has two notes, the unmuted lap one, the muted lap none.
    assert_eq!(state.midi_export(&all, &[], &[], false).unwrap().notes, 3);
    let named = work.path().join("named.mid");
    let muted = laps[0].id.0;
    assert_eq!(
        state
            .midi_export(&named, &[muted], &[], false)
            .unwrap()
            .notes,
        1
    );
}

#[test]
fn midi_files_round_trip_through_export_and_import_with_path_guards() {
    let work = tempfile::tempdir().unwrap();
    let state = IpcState::new();
    let target = CompilingTarget::default();
    let track = add_midi_track_compiled(&state, &target);
    let file = work.path().join("song.mid");

    for bad in ["song.wav", "song", "song.mid.txt"] {
        let error = state
            .midi_export(&work.path().join(bad), &[], &[], false)
            .unwrap_err();
        assert!(error.contains(".mid"), "{error}");
        assert!(
            state
                .midi_import(&work.path().join(bad), None, 0.0, false, &target)
                .is_err()
        );
    }
    let reply = state.midi_export(&file, &[], &[], false).unwrap();
    assert_eq!((reply.tracks, reply.notes), (1, 2));
    let again = state.midi_export(&file, &[], &[], false).unwrap_err();
    assert!(again.contains("overwrite"), "{again}");
    state.midi_export(&file, &[], &[track.0], true).unwrap();
    assert!(state.midi_export(&file, &[9_999], &[], true).is_err());
    assert!(!Path::new(&format!("{}.writing", file.display())).exists());

    // Import onto a new track at beat 8.
    let imported = state.midi_import(&file, None, 8.0, false, &target).unwrap();
    assert_eq!((imported.tracks.len(), imported.clips.len()), (1, 1));
    assert!(!imported.tempo_applied);
    let new_track = TrackId(imported.tracks[0]);
    let clips = clips_on(&state, new_track);
    assert_eq!(clips.len(), 1);
    assert_eq!(
        clips[0].start, 8.0,
        "the music starts at beat 0 plus start_beat"
    );
    let original = notes_of(&clips_on(&state, track)[0]);
    let back = notes_of(&clips[0]);
    assert_eq!(back.len(), 2);
    for (a, b) in original.iter().zip(&back) {
        assert_eq!(a.key, b.key);
        assert!((a.start - b.start).abs() < 0.002 && (a.length - b.length).abs() < 0.002);
    }
    // One undo step for the whole import (track, instrument, clip).
    assert_eq!(
        lock(&state.control)
            .snapshot()
            .history
            .undo_label
            .as_deref(),
        Some("Import MIDI")
    );
    command(&state, &target, UiCommand::Undo);
    assert!(clips_on(&state, new_track).is_empty());

    // Onto the existing track, and a non-MIDI target is refused.
    let onto = state
        .midi_import(&file, Some(track.0), 0.0, false, &target)
        .unwrap();
    assert!(onto.tracks.is_empty());
    assert_eq!(clips_on(&state, track).len(), 2);
    assert!(
        state
            .midi_import(&file, Some(9_999), 0.0, false, &target)
            .is_err()
    );
}
