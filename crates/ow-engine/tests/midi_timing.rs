//! MIDI recording and live input through the mock backends (stage 15): timestamped synthetic MIDI
//! messages injected at seeded random offsets are recorded within one sample of where they were
//! played, across buffer boundaries, a tempo change, loop wraps, a punch range and a count-in;
//! an armed MIDI track plays what arrives, sample-accurately, and only while armed.
//!
//! The engine runs on the `StreamFrames` clock: a message stamped `frame * 1e6 / rate` belongs to
//! that frame of the audio stream however late the mock MIDI thread delivers it, so a test can
//! deliver messages early or late and still know where each must land.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ow_engine::{
    CompileContext, Engine, EngineConfig, MidiClock, MidiInputSelection, MidiTakes, RecordPlan,
    Stopped, TransportCommand,
};
use ow_io::mock::{MockBackend, MockConfig, MockDrive, MockHandle, MockMidiBackend};
use ow_io::{AudioBackend, AudioSelection, BackendRegistry, MidiBackend, StreamRequest};
use ow_model::time::{TempoMap, TempoPoint};
use ow_model::{
    Clip, ClipContent, Device as ModelDevice, EditCommand, Editor, Note, Owned, Project, Strip,
    Track, TrackId, TrackKind,
};

const RATE: u32 = 48_000;
/// At 120 BPM one beat is half a second.
const BEAT: u64 = 24_000;
const PORT: &str = "keys";
/// Frames rendered before a test starts the transport (see the clip test).
const WARM_UP: usize = 128;

struct Rig {
    engine: Engine,
    audio: MockHandle,
    midi: Arc<MockMidiBackend>,
    /// Blocks the stream has rendered so far (messages are delivered by absolute block number).
    blocks: Cell<u64>,
}

fn rig(buffer_frames: u32, capture_frames: usize) -> Rig {
    let backend = MockBackend::new(MockConfig {
        drive: MockDrive::Manual,
        buffer_frames,
        capture_frames,
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
    wait_until("the stream and the MIDI port", || {
        matches!(engine.audio_status().state, ow_io::AudioState::Running)
            && engine.midi().status().ports.iter().all(|p| p.connected)
            && !engine.midi().status().ports.is_empty()
    });
    Rig {
        engine,
        audio,
        midi,
        blocks: Cell::new(0),
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(2));
    }
}

/// A tiny seeded generator (SplitMix64) so the offsets are random but repeatable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// One message to inject: it belongs to stream frame `frame` and is delivered just before the
/// cycle that renders block number `delivered_in`.
#[derive(Clone, Copy)]
struct Injected {
    frame: u64,
    bytes: [u8; 3],
    delivered_in: u64,
}

/// `count` note-on/note-off pairs at random frames in `from..to`, each delivered between one
/// block early and `late` blocks late (never before the start).
fn random_notes(
    rng: &mut Rng,
    count: usize,
    from: u64,
    to: u64,
    block: u64,
    late: u64,
) -> Vec<Injected> {
    let mut out = Vec::new();
    for i in 0..count {
        let on = from + rng.below(to - from - 1_000);
        let off = on + 1 + rng.below(900);
        let key = 36 + (i % 60) as u8;
        for (frame, bytes) in [(on, [0x90, key, 90]), (off, [0x80, key, 0])] {
            let block_of = frame / block;
            let delivered = (block_of + rng.below(late + 2)).saturating_sub(1);
            out.push(Injected {
                frame,
                bytes,
                delivered_in: delivered,
            });
        }
    }
    out
}

fn micros(frame: u64) -> u64 {
    (frame as f64 * 1e6 / f64::from(RATE)).round() as u64
}

/// Runs `blocks` more cycles, injecting each message before the cycle it is delivered in
/// (`delivered_in` counts blocks from the start of the stream).
fn run_injecting(rig: &Rig, injected: &[Injected], blocks: u64) {
    let mut next = 0;
    let mut by_delivery: Vec<&Injected> = injected.iter().collect();
    by_delivery.sort_by_key(|m| m.delivered_in);
    for _ in 0..blocks {
        let block = rig.blocks.get();
        let mut sent = false;
        while next < by_delivery.len() && by_delivery[next].delivered_in <= block {
            let m = by_delivery[next];
            assert!(rig.midi.send(PORT, micros(m.frame), &m.bytes));
            sent = true;
            next += 1;
        }
        if sent {
            rig.midi.flush(PORT);
        }
        rig.audio.run_callbacks(1).unwrap();
        rig.blocks.set(block + 1);
    }
    assert_eq!(next, by_delivery.len(), "every message was delivered");
}

/// Stops the recording while a helper thread keeps the manual device running.
fn stop(rig: &Rig, recording: ow_engine::Recording) -> Stopped {
    let running = Arc::new(AtomicBool::new(true));
    let pump = {
        let running = Arc::clone(&running);
        let audio = rig.audio.clone();
        thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                audio.run_callbacks(1).unwrap();
                thread::sleep(Duration::from_millis(1));
            }
        })
    };
    let stopped = rig.engine.stop_recording(recording).unwrap();
    running.store(false, Ordering::Relaxed);
    pump.join().unwrap();
    stopped
}

fn plan(track: TrackId) -> RecordPlan {
    RecordPlan {
        tracks: Vec::new(),
        midi_tracks: vec![track],
        loop_beats: None,
        punch_beats: None,
        count_in_bars: 0,
    }
}

/// A project with one MIDI track on a `builtin.synth` (a sine with an instant attack) and the
/// given tempo points.
fn project(points: Vec<TempoPoint>, clips: Vec<Clip>) -> (Project, TrackId) {
    let mut editor = Editor::new(Project::new("midi timing"));
    editor
        .apply(EditCommand::SetTempoMap {
            tempo_map: TempoMap::new(points).unwrap(),
        })
        .unwrap();
    let track: TrackId = editor.alloc_id().unwrap();
    let synth: ow_model::DeviceId = editor.alloc_id().unwrap();
    let params = [
        (0, 2.0),
        (1, 20_000.0),
        (2, 0.0),
        (3, 0.001),
        (4, 0.001),
        (5, 1.0),
        (6, 0.01),
        (7, 0.0),
    ];
    let clips = clips
        .into_iter()
        .map(|mut clip| {
            clip.track = track;
            clip.id = editor.alloc_id().unwrap();
            clip
        })
        .collect();
    editor
        .apply(EditCommand::InsertTrack {
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
                clips,
                devices: vec![ModelDevice {
                    id: synth,
                    kind: "builtin.synth".into(),
                    name: "Synth".into(),
                    enabled: true,
                    params: params.into_iter().collect(),
                    state: vec![],
                    sidechain: None,
                }],
                lanes: vec![],
            }),
        })
        .unwrap();
    (editor.project().clone(), track)
}

fn steps(points: &[(f64, f64)]) -> Vec<TempoPoint> {
    points
        .iter()
        .map(|(beat, bpm)| TempoPoint::constant(*beat, *bpm))
        .collect()
}

fn tempo_change() -> Vec<TempoPoint> {
    // 120 BPM for four beats (2 s), then 90.
    steps(&[(0.0, 120.0), (4.0, 90.0)])
}

fn midi_of(stopped: &Stopped) -> &MidiTakes {
    assert!(stopped.complete, "the audio thread confirmed the stop");
    assert_eq!(stopped.midi.dropped_events, 0);
    &stopped.midi
}

fn start_from_beat(rig: &Rig, beat: f64, plan: &RecordPlan) -> ow_engine::Recording {
    assert!(
        rig.engine
            .send_transport(TransportCommand::LocateBeats(beat))
    );
    rig.engine.start_recording_plan(plan).unwrap()
}

#[test]
fn recorded_messages_land_within_a_sample_across_blocks_and_a_tempo_change() {
    for block in [64_u32, 100, 256] {
        let rig = rig(block, 0);
        let (project, track) = project(tempo_change(), vec![]);
        let tempo = project.tempo_map.clone();
        rig.engine
            .set_project(project, CompileContext::default())
            .unwrap();

        // Recording starts at beat 1 (sample 24000) with stream frame 0, so a message for
        // stream frame S belongs at timeline sample 24000 + S. The span runs well past beat 4,
        // where the tempo drops from 120 to 90 BPM.
        let base = BEAT;
        let mut rng = Rng(0x0BAD_5EED + u64::from(block));
        let injected = random_notes(&mut rng, 120, 500, 150_000, u64::from(block), 2);
        let recording = start_from_beat(&rig, 1.0, &plan(track));
        let blocks = 150_000 / u64::from(block) + 20;
        run_injecting(&rig, &injected, blocks);
        let stopped = stop(&rig, recording);

        let midi = midi_of(&stopped);
        assert_eq!(midi.tracks, [track]);
        assert_eq!(midi.passes.len(), 1);
        let pass = &midi.passes[0];
        assert_eq!(pass.start_sample, base, "block {block}");
        assert!(pass.end_sample >= base + 150_000);
        assert_eq!(pass.events.len(), injected.len(), "block {block}");

        // Each message is where it was played (events arrive in frame order per message).
        let mut expected: Vec<(u64, [u8; 3])> =
            injected.iter().map(|m| (base + m.frame, m.bytes)).collect();
        expected.sort_by_key(|(sample, _)| *sample);
        let mut got: Vec<(i64, [u8; 3])> =
            pass.events.iter().map(|e| (e.sample, e.bytes)).collect();
        got.sort_by_key(|(sample, _)| *sample);
        let mut worst = 0_i64;
        for ((want, want_bytes), (sample, bytes)) in expected.iter().zip(&got) {
            worst = worst.max((sample - *want as i64).abs());
            assert_eq!(bytes[0] & 0xF0, want_bytes[0] & 0xF0);
        }
        assert!(worst <= 1, "block {block}: worst error {worst} samples");

        // Through the tempo map the beats agree too: converting the recorded sample to beats
        // and back is within a sample (the map has a change at beat 4).
        for event in &pass.events {
            let beat = tempo.samples_to_beats(event.sample as f64, RATE);
            let back = tempo.beats_to_samples(beat, RATE);
            assert!((back - event.sample as f64).abs() < 1.0);
        }
        assert!(
            pass.events.iter().any(|e| e.sample > 4 * BEAT as i64)
                && pass.events.iter().any(|e| e.sample < 4 * BEAT as i64),
            "the messages span the tempo change"
        );
    }
}

#[test]
fn a_loop_recording_gives_every_lap_its_own_pass_with_exact_positions() {
    for block in [64_u32, 70] {
        let rig = rig(block, 0);
        let (project, track) = project(steps(&[(0.0, 120.0)]), vec![]);
        rig.engine
            .set_project(project, CompileContext::default())
            .unwrap();
        // Loop beats 1 to 2 (24000 frames); stream frame S is lap S / 24000 at loop position
        // S % 24000. The loop wraps in the middle of a block for a block size of 70.
        let lap = BEAT;
        let mut rng = Rng(77);
        let injected = random_notes(&mut rng, 40, 100, 3 * lap - 3_000, u64::from(block), 0);
        let mut recording_plan = plan(track);
        recording_plan.loop_beats = Some((1.0, 2.0));
        let recording = start_from_beat(&rig, 1.0, &recording_plan);
        // Whole blocks that end inside the third lap (a fourth pass would start otherwise).
        let blocks = 3 * lap / u64::from(block);
        run_injecting(&rig, &injected, blocks);
        let stopped = stop(&rig, recording);

        let midi = midi_of(&stopped);
        assert_eq!(midi.passes.len(), 3, "block {block}");
        for (index, pass) in midi.passes.iter().enumerate() {
            assert_eq!(pass.pass as usize, index);
            assert_eq!(
                pass.start_sample, BEAT,
                "every lap starts at the loop start"
            );
            if index < 2 {
                assert_eq!(pass.end_sample, 2 * BEAT, "block {block}, lap {index}");
            } else {
                assert!(
                    pass.end_sample > 2 * BEAT - u64::from(block) && pass.end_sample <= 2 * BEAT
                );
            }
            let want = injected
                .iter()
                .filter(|m| m.frame / lap == index as u64)
                .count();
            assert_eq!(pass.events.len(), want, "block {block}, lap {index}");
            for event in &pass.events {
                let hit = injected.iter().any(|m| {
                    m.frame / lap == index as u64 && {
                        (event.sample - (BEAT + m.frame % lap) as i64).abs() <= 1
                            && event.bytes[0] & 0xF0 == m.bytes[0] & 0xF0
                    }
                });
                assert!(hit, "block {block}, lap {index}: stray event {event:?}");
            }
        }
    }
}

#[test]
fn a_punch_range_and_a_count_in_limit_what_is_recorded() {
    // Punch beats 2 to 3 (samples 48000..72000), recording from beat 1.
    let rig_a = rig(64, 0);
    let (project_a, track) = project(steps(&[(0.0, 120.0)]), vec![]);
    rig_a
        .engine
        .set_project(project_a, CompileContext::default())
        .unwrap();
    let mut punched = plan(track);
    punched.punch_beats = Some((2.0, 3.0));
    let recording = start_from_beat(&rig_a, 1.0, &punched);
    // Stream frame S is timeline 24000 + S: one message before, three inside, one after.
    let at = |timeline: u64| Injected {
        frame: timeline - BEAT,
        bytes: [0x90, 60, 100],
        delivered_in: (timeline - BEAT) / 64,
    };
    let injected = [at(40_000), at(48_000), at(60_000), at(71_999), at(72_000)];
    run_injecting(&rig_a, &injected, 72_000 / 64 + 20);
    let stopped = stop(&rig_a, recording);
    let midi = midi_of(&stopped);
    let samples: Vec<i64> = midi.passes[0].events.iter().map(|e| e.sample).collect();
    assert_eq!(samples, [48_000, 60_000, 71_999]);
    assert_eq!(midi.passes[0].start_sample, 48_000);
    assert_eq!(midi.passes[0].end_sample, 72_000);

    // One bar of count-in (4 beats = 96000 frames): nothing is recorded during it, then the
    // position starts at the recording position.
    let rig_b = rig(64, 0);
    let (project_b, track) = project(steps(&[(0.0, 120.0)]), vec![]);
    rig_b
        .engine
        .set_project(project_b, CompileContext::default())
        .unwrap();
    let mut counted = plan(track);
    counted.count_in_bars = 1;
    let recording = start_from_beat(&rig_b, 1.0, &counted);
    let count_in = 4 * BEAT;
    let injected: Vec<Injected> = [1_000_u64, count_in - 1, count_in, count_in + 1_000]
        .iter()
        .map(|frame| Injected {
            frame: *frame,
            bytes: [0x90, 62, 100],
            delivered_in: frame / 64,
        })
        .collect();
    run_injecting(&rig_b, &injected, (count_in + 2_000) / 64 + 4);
    let stopped = stop(&rig_b, recording);
    let midi = midi_of(&stopped);
    let samples: Vec<i64> = midi.passes[0].events.iter().map(|e| e.sample).collect();
    // The count-in ends at stream frame 96000 and the position is then the start (beat 1).
    assert_eq!(samples, [BEAT as i64, BEAT as i64 + 1_000]);
}

#[test]
fn midi_recording_needs_no_audio_input_and_other_messages_are_captured_raw() {
    let rig = rig(64, 0);
    let (project, track) = project(steps(&[(0.0, 120.0)]), vec![]);
    rig.engine
        .set_project(project, CompileContext::default())
        .unwrap();
    let recording = start_from_beat(&rig, 0.0, &plan(track));
    let injected = [
        Injected {
            frame: 1_000,
            bytes: [0xB0, 64, 127],
            delivered_in: 15,
        },
        Injected {
            frame: 2_000,
            bytes: [0xE0, 0, 96],
            delivered_in: 31,
        },
    ];
    run_injecting(&rig, &injected, 60);
    let stopped = stop(&rig, recording);
    let midi = midi_of(&stopped);
    assert!(stopped.takes.is_empty());
    let bytes: Vec<[u8; 3]> = midi.passes[0].events.iter().map(|e| e.bytes).collect();
    assert_eq!(bytes, [[0xB0, 64, 127], [0xE0, 0, 96]]);
}

fn first_sound(captured: &[f32], from_frame: usize) -> Option<usize> {
    captured
        .chunks(2)
        .enumerate()
        .skip(from_frame)
        .find(|(_, frame)| frame[0].abs() > 1e-4)
        .map(|(index, _)| index)
}

#[test]
fn an_armed_midi_track_plays_live_input_on_the_sample_and_only_while_armed() {
    let rig = rig(64, 48_000 * 4);
    let (project, track) = project(steps(&[(0.0, 120.0)]), vec![]);
    let ctx = CompileContext::default();
    let armed = ctx.armed.clone();
    rig.engine.set_project(project, ctx).unwrap();

    // Not armed: the key press is silent (the transport is stopped throughout).
    let down = Injected {
        frame: 3_100,
        bytes: [0x90, 69, 110],
        delivered_in: 40,
    };
    let up = Injected {
        frame: 6_100,
        bytes: [0x80, 69, 0],
        delivered_in: 90,
    };
    run_injecting(&rig, &[down, up], 120);
    assert!(
        first_sound(&rig.audio.captured(), 0).is_none(),
        "a disarmed track stays silent"
    );

    // Armed (a flag flip, no recompile): the same press sounds from its own frame, delivered
    // some blocks before the frame it is stamped with.
    armed.set(track, true);
    let down = Injected {
        frame: 10_000,
        bytes: [0x90, 69, 110],
        delivered_in: 150,
    };
    let up = Injected {
        frame: 13_000,
        bytes: [0x80, 69, 0],
        delivered_in: 200,
    };
    run_injecting(&rig, &[down, up], 280);
    let captured = rig.audio.captured();
    let heard = first_sound(&captured, 0).expect("the armed track played the note");
    assert!(
        (10_000..=10_003).contains(&heard),
        "the note belongs to frame 10000 and was first heard at {heard}"
    );
    // The note-off releases it (the synth's release is 10 ms), after which it is silent again.
    assert!(first_sound(&captured, 13_000 + 1_200).is_none(), "released");
}

#[test]
fn disarming_releases_a_key_that_is_still_held() {
    let rig = rig(64, 48_000);
    let (project, track) = project(steps(&[(0.0, 120.0)]), vec![]);
    let ctx = CompileContext::default();
    let armed = ctx.armed.clone();
    rig.engine.set_project(project, ctx).unwrap();
    armed.set(track, true);
    let down = Injected {
        frame: 640,
        bytes: [0x90, 60, 100],
        delivered_in: 5,
    };
    run_injecting(&rig, &[down], 40);
    assert!(first_sound(&rig.audio.captured(), 0).is_some());
    armed.set(track, false);
    rig.audio.run_callbacks(60).unwrap();
    // The key was never lifted, yet after the release time nothing sounds.
    assert!(first_sound(&rig.audio.captured(), 40 * 64 + 1_500).is_none());
}

#[test]
fn a_clip_note_and_a_live_note_play_together() {
    let rig = rig(64, 48_000);
    let clip = Clip {
        id: 0.into(),
        track: 0.into(),
        name: "clip".into(),
        start: 0.0,
        length: 1.0,
        muted: false,
        content: ClipContent::Midi {
            notes: vec![Note {
                start: 0.0,
                length: 0.5,
                key: 48,
                velocity: 100,
            }],
            controllers: vec![],
        },
        take: None,
    };
    let (project, track) = project(steps(&[(0.0, 120.0)]), vec![clip]);
    let ctx = CompileContext::default();
    let armed = ctx.armed.clone();
    rig.engine.set_project(project, ctx).unwrap();
    armed.set(track, true);
    // The audio thread swaps in one plan per cycle (the smoke plan, then the project's), so the
    // transport starts after two blocks.
    rig.audio.run_callbacks(2).unwrap();
    rig.blocks.set(2);
    assert!(rig.engine.send_transport(TransportCommand::Play));
    let live = Injected {
        frame: 20_000,
        bytes: [0x90, 72, 100],
        delivered_in: 100,
    };
    run_injecting(&rig, &[live], 400);
    let captured = rig.audio.captured();
    // The clip note sounds from the first frame of playback and ends 12000 frames later (plus
    // its release); the live note is what remains, from its own frame.
    let first = first_sound(&captured, 0);
    assert!(
        first.is_some_and(|f| (WARM_UP..=WARM_UP + 3).contains(&f)),
        "the clip note starts with the transport at frame {WARM_UP}; heard at {first:?}"
    );
    let later = first_sound(&captured, 13_000).expect("the live note sounds");
    assert!((20_000..=20_003).contains(&later), "heard at {later}");
}
