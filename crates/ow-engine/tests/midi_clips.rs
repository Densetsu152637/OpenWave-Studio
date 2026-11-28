//! MIDI clips driving the built-in synth through the compiler and the engine cycle (MVP goal
//! M-G7): a note-on lands on the sample its beat position maps to, for every block size and
//! tempo; notes are cut at the clip end; stop, locate and loop wraps release sounding notes; the
//! stereo device node also works as an effect.

use ow_dsp::{Device, DeviceInfo, Event, EventKind};
use ow_engine::clips::midi::{MidiClipPlayer, ScheduledEvent, schedule};
use ow_engine::{
    CompileContext, DeviceNode, Graph, MAX_BLOCK, Message, Node, OfflineRenderer, ProcessContext,
    Timeline, TransportCommand, compile_project,
};
use ow_model::time::TempoMap;
use ow_model::{
    Clip, ClipContent, Device as ModelDevice, EditCommand, Editor, Note, Owned, Project, Strip,
    Track, TrackKind,
};

const RATE: u32 = 48_000;
/// At the default 120 BPM one beat is half a second.
const BEAT: u64 = 24_000;

fn note(start: f64, length: f64, key: u8) -> Note {
    Note {
        start,
        length,
        key,
        velocity: 100,
    }
}

fn midi_clip(id: u64, track: u64, start: f64, length: f64, notes: Vec<Note>) -> Clip {
    Clip {
        id: id.into(),
        track: track.into(),
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

/// A MIDI track playing `clips` (built from the track's id) on a `builtin.synth` set to a sine
/// with an instant attack, wide-open filter and a short release, so onsets are easy to find.
fn project_with(tempo_bpm: f64, clips: impl FnOnce(u64) -> Vec<Clip>) -> Project {
    let mut editor = Editor::new(Project::new("midi"));
    editor
        .apply(EditCommand::SetTempoMap {
            tempo_map: TempoMap::constant(tempo_bpm).unwrap(),
        })
        .unwrap();
    let track = editor.alloc_id::<ow_model::TrackId>().unwrap();
    let synth = editor.alloc_id::<ow_model::DeviceId>().unwrap();
    let clips = clips(track.0);
    for clip in &clips {
        // Reserve each clip's id so the model's counter stays above it.
        while editor.project().next_id <= clip.id.0 {
            let _ = editor.alloc_id::<ow_model::ClipId>().unwrap();
        }
    }
    let params = [
        (0, 2.0),      // sine
        (1, 20_000.0), // cutoff
        (2, 0.0),      // resonance
        (3, 0.001),    // attack
        (4, 0.001),    // decay
        (5, 1.0),      // sustain
        (6, 0.01),     // release
        (7, 0.0),      // gain
    ];
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
    editor.project().clone()
}

/// The song's left channel, rendered in blocks of `block` frames from the start.
fn render(project: &Project, frames: usize, block: usize) -> Vec<f32> {
    let compiled = compile_project(project, &CompileContext::default(), RATE, MAX_BLOCK).unwrap();
    let mut renderer = OfflineRenderer::new(compiled.plan, 2);
    assert!(renderer.link().swap_timeline(compiled.timeline).is_ok());
    assert!(renderer.send(Message::Transport(TransportCommand::Play)));
    renderer
        .render(frames, block)
        .chunks(2)
        .map(|frame| frame[0])
        .collect()
}

#[test]
fn midi_note_on_lands_on_the_sample_its_beat_position_maps_to() {
    // One clip starting at beat 1 with a note half a beat in: the note sounds from beat 1.5.
    for bpm in [120.0, 133.3, 97.0, 174.5] {
        let project = project_with(bpm, |track| {
            vec![midi_clip(100, track, 1.0, 4.0, vec![note(0.5, 1.0, 69)])]
        });
        let timeline = Timeline::new(
            project.tempo_map.clone(),
            project.time_signature_map.clone(),
        );
        let on = timeline.beat_to_sample_index(1.5, RATE) as usize;
        let reference = render(&project, 100_000, 4096);
        let first = reference.iter().position(|s| *s != 0.0).expect("it sounds");
        assert_eq!(first, on, "at {bpm} bpm");
        // The same render at awkward block sizes is bit-identical.
        for block in [1, 7, 64, 100, 333, 1024] {
            assert_eq!(render(&project, 100_000, block), reference, "block {block}");
        }
    }
}

#[test]
fn midi_note_stops_at_the_note_end_and_never_beyond_its_clip() {
    // Note 0: beat 0 to 1 (sounds 0..24000, then a 10 ms release). Note 1 starts at beat 3 and
    // is 4 beats long but its clip ends at beat 4: it is cut there.
    let project = project_with(120.0, |track| {
        vec![midi_clip(
            100,
            track,
            0.0,
            4.0,
            vec![note(0.0, 1.0, 69), note(3.0, 4.0, 72)],
        )]
    });
    let out = render(&project, 6 * BEAT as usize, 512);
    let peak = |from: u64, to: u64| {
        out[from as usize..to as usize]
            .iter()
            .fold(0.0_f32, |m, s| m.max(s.abs()))
    };
    assert!(peak(1000, BEAT) > 0.5, "first note sounds");
    assert_eq!(peak(BEAT + 2000, 3 * BEAT), 0.0, "silent between the notes");
    assert!(peak(3 * BEAT + 1000, 4 * BEAT) > 0.5, "second note sounds");
    assert_eq!(peak(4 * BEAT + 2000, 6 * BEAT), 0.0, "cut at the clip end");
}

#[test]
fn midi_schedule_orders_clips_and_drops_what_cannot_sound() {
    let timeline = Timeline::default();
    let mut muted = midi_clip(2, 1, 0.0, 4.0, vec![note(0.0, 1.0, 40)]);
    muted.muted = true;
    let audio_like = Clip {
        content: ClipContent::Audio {
            media: 9.into(),
            source_offset: 0,
            gain_db: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            warp: None,
        },
        ..midi_clip(3, 1, 0.0, 4.0, vec![])
    };
    let clips = [
        // A note starting at the clip's end is dropped; a repeated key retriggers (off, then on).
        midi_clip(
            1,
            1,
            2.0,
            1.0,
            vec![note(0.0, 0.5, 60), note(0.5, 0.5, 60), note(1.0, 1.0, 61)],
        ),
        muted,
        audio_like,
        midi_clip(4, 1, 0.0, 1.0, vec![note(0.0, 0.00001, 50)]),
    ];
    let events = schedule(clips.iter(), &timeline, RATE);
    let at = |beat: u64| beat * BEAT;
    assert_eq!(
        events,
        vec![
            ScheduledEvent {
                sample: 0,
                key: 50,
                velocity: 100
            },
            // Shorter than a sample: it still lasts one.
            ScheduledEvent {
                sample: 1,
                key: 50,
                velocity: 0
            },
            ScheduledEvent {
                sample: at(2),
                key: 60,
                velocity: 100
            },
            ScheduledEvent {
                sample: at(2) + BEAT / 2,
                key: 60,
                velocity: 0
            },
            ScheduledEvent {
                sample: at(2) + BEAT / 2,
                key: 60,
                velocity: 100
            },
            ScheduledEvent {
                sample: at(3),
                key: 60,
                velocity: 0
            },
        ]
    );
}

/// A node that plays a [`MidiClipPlayer`] and writes what it emits as impulses: note-ons on
/// channel 0, note-offs on channel 1, so tests can read event frames out of rendered audio.
struct Recorder {
    player: MidiClipPlayer,
    events: Vec<Event>,
}

impl Node for Recorder {
    fn name(&self) -> &'static str {
        "recorder"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        2
    }
    fn prepare(&mut self, _rate: u32, _max_block: usize) {
        self.events = Vec::with_capacity(4096);
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        self.events.clear();
        self.player.generate(&ctx.timing, &mut self.events);
        ctx.outputs.channel_mut(0).fill(0.0);
        ctx.outputs.channel_mut(1).fill(0.0);
        let mut previous = 0;
        for event in &self.events {
            assert!(event.offset >= previous, "events are in frame order");
            assert!(
                (event.offset as usize) < ctx.frames,
                "offset inside the block"
            );
            previous = event.offset;
            let channel = usize::from(matches!(event.kind, EventKind::NoteOff { .. }));
            ctx.outputs.channel_mut(channel)[event.offset as usize] += 1.0;
        }
    }
}

/// Runs `events` through a [`Recorder`] and returns the frames of the note-ons and note-offs in
/// `frames` rendered frames, after the `setup` messages (sent before the first block).
fn record(
    events: Vec<ScheduledEvent>,
    messages: Vec<Message>,
    frames: usize,
    block: usize,
) -> (Vec<usize>, Vec<usize>) {
    record_with(events, messages, frames, block, |_| {})
}

fn record_with(
    events: Vec<ScheduledEvent>,
    messages: Vec<Message>,
    frames: usize,
    block: usize,
    mid_render: impl FnOnce(&mut OfflineRenderer),
) -> (Vec<usize>, Vec<usize>) {
    let mut graph = Graph::new();
    let recorder = graph.add_node(Box::new(Recorder {
        player: MidiClipPlayer::new(events),
        events: Vec::new(),
    }));
    let master = graph.add_node(Box::new(ow_engine::MasterNode::new()));
    graph.connect(recorder, 0, master, 0).unwrap();
    graph.connect(recorder, 1, master, 1).unwrap();
    graph.set_master(master).unwrap();
    let mut renderer = OfflineRenderer::new(graph.compile(RATE, MAX_BLOCK).unwrap(), 2);
    for message in messages {
        assert!(renderer.send(message));
    }
    let mut out = renderer.render(frames / 2, block);
    mid_render(&mut renderer);
    out.extend(renderer.render(frames - frames / 2, block));
    let frames_with = |channel: usize| {
        out.chunks(2)
            .enumerate()
            .filter(|(_, f)| f[channel] != 0.0)
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    };
    (frames_with(0), frames_with(1))
}

fn on(sample: u64, key: u8) -> ScheduledEvent {
    ScheduledEvent {
        sample,
        key,
        velocity: 90,
    }
}

fn off(sample: u64, key: u8) -> ScheduledEvent {
    ScheduledEvent {
        sample,
        key,
        velocity: 0,
    }
}

#[test]
fn midi_events_land_on_their_samples_for_every_block_size() {
    let events = vec![
        on(0, 60),
        off(63, 60),
        on(64, 61),
        off(1_000, 61),
        on(4_095, 62),
        on(4_096, 63),
        off(4_097, 62),
        off(9_999, 63),
    ];
    let want_on = [0, 64, 4_095, 4_096];
    let want_off = [63, 1_000, 4_097, 9_999];
    for block in [1, 2, 5, 63, 64, 65, 100, 512, 4_095, 4_096] {
        let (ons, offs) = record(
            events.clone(),
            vec![Message::Transport(TransportCommand::Play)],
            12_000,
            block,
        );
        assert_eq!(ons, want_on, "block {block}");
        assert_eq!(offs, want_off, "block {block}");
    }
}

#[test]
fn midi_nothing_plays_while_stopped_and_a_stop_releases_the_sounding_notes() {
    let events = vec![on(100, 60), off(50_000, 60)];
    let (ons, offs) = record(events.clone(), vec![], 4_000, 256);
    assert!(ons.is_empty() && offs.is_empty(), "stopped: no events");

    // Playing, then stopped at the halfway point (frame 2 000): the note is released then, not
    // at its own end, which is never reached.
    let (ons, offs) = record_with(
        events,
        vec![Message::Transport(TransportCommand::Play)],
        4_000,
        256,
        |renderer| {
            assert!(renderer.send(Message::Transport(TransportCommand::Stop)));
        },
    );
    assert_eq!(ons, [100]);
    // The stop applies at the start of the next block, frame 2 000, and releases the note there.
    assert_eq!(offs, [2_000]);
}

#[test]
fn midi_locate_releases_notes_and_a_note_at_the_new_position_starts() {
    let events = vec![
        on(1_000, 60),
        off(40_000, 60),
        on(30_000, 64),
        off(31_000, 64),
    ];
    let mut events = events;
    events.sort_by_key(|e| e.sample);
    // After 3 000 frames the transport jumps to sample 29 500: note 60 is released at the jump,
    // and note 64 starts 500 frames later.
    let (ons, offs) = record_with(
        events,
        vec![Message::Transport(TransportCommand::Play)],
        8_000,
        100,
        |renderer| {
            assert!(renderer.send(Message::Transport(TransportCommand::Locate(29_500))));
        },
    );
    // First half is 4 000 frames: the locate applies at frame 4 000 (a block boundary).
    assert_eq!(ons[0], 1_000);
    assert_eq!(offs[0], 4_000, "released at the locate: {offs:?}");
    assert_eq!(ons[1], 4_000 + 500, "the note at the new position: {ons:?}");
    assert_eq!(offs[1], 4_000 + 1_500);
    assert_eq!(ons.len(), 2);
    assert_eq!(offs.len(), 2);
}

#[test]
fn midi_loop_wrap_releases_notes_and_retriggers_from_the_loop_start() {
    // Loop [0, 2) beats = 48 000 samples. A note from 40 000 to 60 000 crosses the loop end: it
    // is released at the wrap. A note at sample 0 plays on every pass.
    let events = vec![on(0, 60), off(10_000, 60), on(40_000, 62), off(60_000, 62)];
    let (ons, offs) = record(
        events,
        vec![
            Message::Transport(TransportCommand::SetLoop {
                enabled: true,
                start_beats: 0.0,
                end_beats: 2.0,
            }),
            Message::Transport(TransportCommand::Play),
        ],
        120_000,
        480,
    );
    assert_eq!(ons, [0, 40_000, 48_000, 88_000, 96_000]);
    // Each pass: off at 10 000 (key 60), and the wrap release of key 62 at the next pass start.
    assert_eq!(offs, [10_000, 48_000, 58_000, 96_000, 106_000]);
}

#[test]
fn midi_a_single_loop_that_is_shorter_than_a_block_still_places_every_event() {
    // Loop of 0.5 beat = 12 000 samples inside blocks of 4 096: spans restart mid-block.
    let events = vec![on(5_000, 60), off(6_000, 60)];
    let (ons, offs) = record(
        events,
        vec![
            Message::Transport(TransportCommand::SetLoop {
                enabled: true,
                start_beats: 0.0,
                end_beats: 0.5,
            }),
            Message::Transport(TransportCommand::Play),
        ],
        36_000,
        4_096,
    );
    assert_eq!(ons, [5_000, 17_000, 29_000]);
    assert_eq!(offs, [6_000, 18_000, 30_000]);
}

/// An effect device used to prove the node works as a stereo insert: doubles the left channel
/// and halves the right.
struct Skew;

impl Device for Skew {
    fn prepare(&mut self, _rate: f32, _max_block: usize) {}
    fn set_param(&mut self, _id: u32, _value: f64) {}
    fn reset(&mut self) {}
    fn process(&mut self, audio: &mut [&mut [f32]; 2], _events: &[Event]) {
        audio[0].iter_mut().for_each(|s| *s *= 2.0);
        audio[1].iter_mut().for_each(|s| *s *= 0.5);
    }
}

#[test]
fn midi_device_node_runs_as_a_stereo_effect() {
    static INFO: DeviceInfo = DeviceInfo::new("test.skew", "Skew", false, &[], || Box::new(Skew));
    let mut graph = Graph::new();
    let source = graph.add_node(Box::new(ow_engine::OscillatorNode::new(440.0, 0.25)));
    let effect = graph.add_node(Box::new(DeviceNode::effect(&INFO, Box::new(Skew))));
    let master = graph.add_node(Box::new(ow_engine::MasterNode::new()));
    graph.connect(source, 0, effect, 0).unwrap();
    graph.connect(source, 0, effect, 1).unwrap();
    graph.connect(effect, 0, master, 0).unwrap();
    graph.connect(effect, 1, master, 1).unwrap();
    graph.set_master(master).unwrap();
    let mut renderer = OfflineRenderer::new(graph.compile(RATE, MAX_BLOCK).unwrap(), 2);
    renderer.send(Message::Transport(TransportCommand::Play));
    let out = renderer.render(4_800, 256);
    let peak = |channel: usize| out.chunks(2).fold(0.0_f32, |m, f| m.max(f[channel].abs()));
    assert!((peak(0) - 0.5).abs() < 0.01, "left doubled: {}", peak(0));
    assert!((peak(1) - 0.125).abs() < 0.01, "right halved: {}", peak(1));
}

#[test]
fn midi_render_is_identical_with_and_without_dsp_workers() {
    let project = project_with(120.0, |track| {
        vec![midi_clip(
            100,
            track,
            0.0,
            8.0,
            (0..16)
                .map(|i| note(f64::from(i) * 0.5, 0.75, 48 + i as u8))
                .collect(),
        )]
    });
    let compiled = |workers| {
        let compiled =
            compile_project(&project, &CompileContext::default(), RATE, MAX_BLOCK).unwrap();
        let mut renderer = OfflineRenderer::with_workers(compiled.plan, 2, workers);
        assert!(renderer.link().swap_timeline(compiled.timeline).is_ok());
        renderer.send(Message::Transport(TransportCommand::Play));
        renderer.render(3 * BEAT as usize, 256)
    };
    let serial = compiled(0);
    assert!(serial.iter().any(|s| s.abs() > 0.1));
    assert_eq!(compiled(2), serial);
}

#[test]
fn midi_export_renders_the_notes_and_their_release_inside_the_tail() {
    use ow_engine::export::{TAIL_SECONDS, export_frames, render_project};
    // One bar (4 beats = 2 s) with a note over the whole bar.
    let project = project_with(120.0, |track| {
        vec![midi_clip(100, track, 0.0, 4.0, vec![note(0.0, 4.0, 60)])]
    });
    let frames = export_frames(&project) as usize;
    assert_eq!(frames, (2 + TAIL_SECONDS as usize) * RATE as usize);
    let mix = render_project(&project, &CompileContext::default(), frames).unwrap();
    let left: Vec<f32> = mix.chunks(2).map(|f| f[0]).collect();
    assert!(
        left[1_000..90_000].iter().any(|s| s.abs() > 0.1),
        "the note sounds"
    );
    // The 10 ms release is long over a quarter-second after the clip's end.
    assert!(left[2 * RATE as usize + 12_000..].iter().all(|s| *s == 0.0));
}

/// An instrument that logs the events it is given.
struct Logger(std::sync::Arc<std::sync::Mutex<Vec<Event>>>);

impl Device for Logger {
    fn prepare(&mut self, _rate: f32, _max_block: usize) {}
    fn set_param(&mut self, _id: u32, _value: f64) {}
    fn reset(&mut self) {}
    fn process(&mut self, _audio: &mut [&mut [f32]; 2], events: &[Event]) {
        self.0.lock().unwrap().extend_from_slice(events);
    }
}

/// A live note-off and a clip note-off and note-on of one key on the same frame reach the
/// device with both note-offs first, in frame order, whatever the sort does with ties.
#[test]
fn same_frame_live_and_clip_events_keep_note_offs_before_note_ons() {
    use ow_engine::{CoreBuilder, LiveGate, MidiClock, MidiMessage};
    static INFO: DeviceInfo =
        DeviceInfo::new("test.logger", "Logger", true, &[], || Box::new(Skew));
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events = vec![on(40, 60), off(100, 60), on(100, 60)];
    let mut graph = Graph::new();
    let node = DeviceNode::instrument(
        &INFO,
        Box::new(Logger(std::sync::Arc::clone(&log))),
        MidiClipPlayer::new(events),
    )
    .with_live_input(ow_engine::LiveGate::Always);
    let _ = LiveGate::Always;
    let node = graph.add_node(Box::new(node));
    let master = graph.add_node(Box::new(ow_engine::MasterNode::new()));
    graph.connect(node, 0, master, 0).unwrap();
    graph.connect(node, 1, master, 1).unwrap();
    graph.set_master(master).unwrap();
    let mut builder = CoreBuilder::new().midi_clock(MidiClock::StreamFrames);
    let mut midi = builder.producer_ring(16);
    let (mut link, mut core) = builder.build();
    link.swap_plan(graph.compile(RATE, MAX_BLOCK).unwrap())
        .unwrap();
    let stamp = |frame: u64| (frame as f64 * 1e6 / f64::from(RATE)).round() as u64;
    for (frame, status, key) in [(5, 0x90, 60), (100, 0x80, 60), (100, 0x90, 61)] {
        midi.push(Message::MidiIn {
            message: MidiMessage {
                status,
                data1: key,
                data2: 90,
            },
            timestamp_us: stamp(frame),
        });
    }
    assert!(link.send(Message::Transport(TransportCommand::Play)));
    let mut buffer = vec![0.0_f32; 256];
    core.process(&mut buffer, 2, None);
    let log = log.lock().unwrap();
    let at_100: Vec<EventKind> = log
        .iter()
        .filter(|e| e.offset == 100)
        .map(|e| e.kind)
        .collect();
    assert_eq!(at_100.len(), 4, "{log:?}");
    let first_on = at_100
        .iter()
        .position(|k| matches!(k, EventKind::NoteOn { .. }))
        .unwrap();
    assert!(
        at_100[first_on..]
            .iter()
            .all(|k| matches!(k, EventKind::NoteOn { .. })),
        "note-offs first: {at_100:?}"
    );
    assert!(log.windows(2).all(|w| w[0].offset <= w[1].offset));
}

/// A live key tapped within one frame (note-on and note-off stamped alike) is released in the
/// next block, and a clip note shorter than a sample still gets its off after its on.
#[test]
fn a_same_frame_live_tap_and_a_very_short_clip_note_never_stick() {
    use ow_engine::{CoreBuilder, MidiClock, MidiMessage};
    static INFO: DeviceInfo =
        DeviceInfo::new("test.logger2", "Logger", true, &[], || Box::new(Skew));
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    // The clip note is shorter than one sample: the player gives it one.
    let events = schedule(
        [&Clip {
            id: 1.into(),
            track: 1.into(),
            name: "c".into(),
            start: 0.0,
            length: 1.0,
            muted: false,
            content: ClipContent::Midi {
                notes: vec![Note {
                    start: 0.001,
                    length: 1e-9,
                    key: 50,
                    velocity: 90,
                }],
                controllers: vec![],
            },
            take: None,
        }],
        &Timeline::default(),
        RATE,
    );
    assert!(events[1].sample > events[0].sample);
    let mut graph = Graph::new();
    let node = DeviceNode::instrument(
        &INFO,
        Box::new(Logger(std::sync::Arc::clone(&log))),
        MidiClipPlayer::new(events),
    )
    .with_live_input(ow_engine::LiveGate::Always);
    let node = graph.add_node(Box::new(node));
    let master = graph.add_node(Box::new(ow_engine::MasterNode::new()));
    graph.connect(node, 0, master, 0).unwrap();
    graph.connect(node, 1, master, 1).unwrap();
    graph.set_master(master).unwrap();
    let mut builder = CoreBuilder::new().midi_clock(MidiClock::StreamFrames);
    let mut midi = builder.producer_ring(16);
    let (mut link, mut core) = builder.build();
    link.swap_plan(graph.compile(RATE, MAX_BLOCK).unwrap())
        .unwrap();
    let stamp = (50.0 * 1e6 / f64::from(RATE)).round() as u64;
    for status in [0x90, 0x80] {
        midi.push(Message::MidiIn {
            message: MidiMessage {
                status,
                data1: 61,
                data2: 90,
            },
            timestamp_us: stamp,
        });
    }
    assert!(link.send(Message::Transport(TransportCommand::Play)));
    let mut buffer = vec![0.0_f32; 128];
    core.process(&mut buffer, 2, None);
    core.process(&mut buffer, 2, None);
    let log = log.lock().unwrap();
    let key = |k: u8| -> Vec<bool> {
        log.iter()
            .filter_map(|e| match e.kind {
                EventKind::NoteOn { key, .. } if key == k => Some(true),
                EventKind::NoteOff { key } if key == k => Some(false),
                _ => None,
            })
            .collect()
    };
    assert_eq!(key(61), [true, false], "the tap is released: {log:?}");
    assert_eq!(key(50), [true, false], "the short clip note ends: {log:?}");
}

/// The deferred off of a same-frame tap meets a new note-on of the same key at frame 0 of the
/// next block: the key retriggers (off, then on) instead of staying silent.
#[test]
fn a_deferred_live_off_comes_before_a_note_on_at_frame_zero_of_the_next_block() {
    use ow_engine::{CoreBuilder, MidiClock, MidiMessage};
    static INFO: DeviceInfo =
        DeviceInfo::new("test.logger3", "Logger", true, &[], || Box::new(Skew));
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut graph = Graph::new();
    let node = DeviceNode::instrument(
        &INFO,
        Box::new(Logger(std::sync::Arc::clone(&log))),
        MidiClipPlayer::new(vec![]),
    )
    .with_live_input(ow_engine::LiveGate::Always);
    let node = graph.add_node(Box::new(node));
    let master = graph.add_node(Box::new(ow_engine::MasterNode::new()));
    graph.connect(node, 0, master, 0).unwrap();
    graph.connect(node, 1, master, 1).unwrap();
    graph.set_master(master).unwrap();
    let mut builder = CoreBuilder::new().midi_clock(MidiClock::StreamFrames);
    let mut midi = builder.producer_ring(16);
    let (mut link, mut core) = builder.build();
    link.swap_plan(graph.compile(RATE, MAX_BLOCK).unwrap())
        .unwrap();
    let stamp = |frame: u64| (frame as f64 * 1e6 / f64::from(RATE)).round() as u64;
    // Blocks of 64 frames: a tap on frame 50, then the key pressed again on frame 64.
    for (frame, status) in [(50, 0x90), (50, 0x80), (64, 0x90)] {
        midi.push(Message::MidiIn {
            message: MidiMessage {
                status,
                data1: 61,
                data2: 90,
            },
            timestamp_us: stamp(frame),
        });
    }
    let mut buffer = vec![0.0_f32; 128];
    core.process(&mut buffer, 2, None);
    core.process(&mut buffer, 2, None);
    let log = log.lock().unwrap();
    let kinds: Vec<(u32, bool)> = log
        .iter()
        .map(|e| (e.offset, matches!(e.kind, EventKind::NoteOn { .. })))
        .collect();
    assert_eq!(kinds, [(50, true), (0, false), (0, true)], "{log:?}");
}

/// A graph with one live-input logger instrument; returns the shared log, the core and the
/// producer of live MIDI.
fn live_logger(
    info: &'static DeviceInfo,
    gate: ow_engine::LiveGate,
) -> (
    std::sync::Arc<std::sync::Mutex<Vec<Event>>>,
    ow_engine::AudioCore,
    ow_engine::RingSender<Message>,
    ow_engine::ControlLink,
) {
    use ow_engine::{CoreBuilder, MidiClock};
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut graph = Graph::new();
    let node = DeviceNode::instrument(
        info,
        Box::new(Logger(std::sync::Arc::clone(&log))),
        MidiClipPlayer::new(vec![]),
    )
    .with_live_input(gate);
    let node = graph.add_node(Box::new(node));
    let master = graph.add_node(Box::new(ow_engine::MasterNode::new()));
    graph.connect(node, 0, master, 0).unwrap();
    graph.connect(node, 1, master, 1).unwrap();
    graph.set_master(master).unwrap();
    let mut builder = CoreBuilder::new().midi_clock(MidiClock::StreamFrames);
    let midi = builder.producer_ring(2048);
    let (mut link, core) = builder.build();
    link.swap_plan(graph.compile(RATE, MAX_BLOCK).unwrap())
        .unwrap();
    (log, core, midi, link)
}

/// A burst of live controller changes that fills the buffer cannot crowd out a note-off: the
/// key held from the block before is still released.
#[test]
fn a_controller_burst_does_not_crowd_out_a_live_note_off() {
    use ow_engine::MidiMessage;
    static INFO: DeviceInfo =
        DeviceInfo::new("test.logger4", "Logger", true, &[], || Box::new(Skew));
    let (log, mut core, mut midi, _link) = live_logger(&INFO, ow_engine::LiveGate::Always);
    let stamp = |frame: u64| (frame as f64 * 1e6 / f64::from(RATE)).round() as u64;
    let msg = |status, data1, data2, frame| Message::MidiIn {
        message: MidiMessage {
            status,
            data1,
            data2,
        },
        timestamp_us: stamp(frame),
    };
    midi.push(msg(0x90, 61, 90, 5));
    let mut buffer = vec![0.0_f32; 128];
    core.process(&mut buffer, 2, None);
    // Block two: far more controller changes than the buffer holds, then the note-off.
    for i in 0..1500_u64 {
        midi.push(msg(0xB0, 1, (i % 128) as u8, 64 + i % 60));
    }
    midi.push(msg(0x80, 61, 0, 125));
    core.process(&mut buffer, 2, None);
    let log = log.lock().unwrap();
    assert!(
        log.iter()
            .any(|e| matches!(e.kind, EventKind::NoteOff { key: 61 })),
        "the note-off arrived"
    );
    let controllers = log
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ControlChange { .. }))
        .count();
    assert!(controllers <= 512, "{controllers} controller events kept");
}

/// A gate that closes while the live input holds the sustain pedal down lifts the pedal (and
/// releases the held key) instead of waiting for a physical lift.
#[test]
fn closing_the_gate_lifts_a_forwarded_pedal() {
    use ow_engine::MidiMessage;
    static INFO: DeviceInfo =
        DeviceInfo::new("test.logger5", "Logger", true, &[], || Box::new(Skew));
    let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (log, mut core, mut midi, _link) = live_logger(
        &INFO,
        ow_engine::LiveGate::Armed(std::sync::Arc::clone(&armed)),
    );
    let stamp = |frame: u64| (frame as f64 * 1e6 / f64::from(RATE)).round() as u64;
    for (status, data1, data2, frame) in [(0xB0, 64, 127, 4), (0x90, 61, 90, 8)] {
        midi.push(Message::MidiIn {
            message: MidiMessage {
                status,
                data1,
                data2,
            },
            timestamp_us: stamp(frame),
        });
    }
    let mut buffer = vec![0.0_f32; 128];
    core.process(&mut buffer, 2, None);
    armed.store(false, std::sync::atomic::Ordering::Relaxed);
    core.process(&mut buffer, 2, None);
    let log = log.lock().unwrap();
    let events: Vec<(u32, EventKind)> = log.iter().map(|e| (e.offset, e.kind)).collect();
    assert_eq!(
        events,
        [
            (
                4,
                EventKind::ControlChange {
                    controller: 64,
                    value: 127
                }
            ),
            (
                8,
                EventKind::NoteOn {
                    key: 61,
                    velocity: 90
                }
            ),
            (
                0,
                EventKind::ControlChange {
                    controller: 64,
                    value: 0
                }
            ),
            (0, EventKind::NoteOff { key: 61 }),
        ],
        "{log:?}"
    );
}
