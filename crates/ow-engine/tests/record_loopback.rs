//! Recording through the mock backend (MVP goal M-G6): the engine plays a project with one
//! impulse clip, the mock device feeds the output back to its input after a set latency, and a
//! take recorded on an armed track must hold that impulse where it was played, to the sample,
//! after latency compensation. Takes hold every frame the tap pushed (no missing tail).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ow_engine::{CompileContext, Engine, EngineConfig, RecordError, TransportCommand};
use ow_io::mock::{MockBackend, MockConfig, MockDrive, MockMidiBackend};
use ow_io::{AudioBackend, AudioSelection, BackendRegistry, MidiBackend, StreamRequest};
use ow_model::{
    Clip, ClipContent, EditCommand, Editor, MediaItem, Owned, Project, Strip, Track, TrackId,
    TrackKind,
};

const RATE: u32 = 48_000;
/// At the default 120 BPM one beat is half a second.
const BEAT: u64 = 24_000;
/// Round-trip latency of the mock device in frames.
const LATENCY: u32 = 480;
/// Where the impulse sits inside the media file.
const IMPULSE_AT: u64 = 100;
/// The impulse clip starts at beat 2: timeline sample `2 * BEAT + IMPULSE_AT`.
const IMPULSE_POSITION: u64 = 2 * BEAT + IMPULSE_AT;

fn engine(input_channels: u16) -> (Engine, MockBackend) {
    let backend = MockBackend::new(MockConfig {
        drive: MockDrive::Thread { paced: true },
        buffer_frames: 64,
        input_channels,
        loopback_latency_frames: (input_channels > 0).then_some(LATENCY),
        ..MockConfig::default()
    });
    let registry = BackendRegistry::with(vec![Arc::new(backend.clone()) as Arc<dyn AudioBackend>]);
    let config = EngineConfig {
        audio: AudioSelection {
            backend: "mock".into(),
            request: StreamRequest {
                input_device: (input_channels > 0).then(|| "default".into()),
                ..StreamRequest::default()
            },
        },
        ..EngineConfig::default()
    };
    let midi: Arc<dyn MidiBackend> = Arc::new(MockMidiBackend::new([] as [&str; 0]));
    (Engine::with_backends(registry, midi, config), backend)
}

fn add_track(editor: &mut Editor, name: &str) -> TrackId {
    let id: TrackId = editor.alloc_id().unwrap();
    let index = editor.project().tracks.len();
    editor
        .apply(EditCommand::InsertTrack {
            index,
            bundle: Box::new(Owned::new(Track {
                id,
                name: name.into(),
                color: 0,
                kind: TrackKind::Audio,
                strip: Strip::default(),
                instrument: None,
                input: None,
                monitor: Default::default(),
            })),
        })
        .unwrap();
    id
}

/// A project with a track that plays an impulse at [`IMPULSE_POSITION`] and an empty track to
/// record on; its media root holds the impulse file.
fn project(root: &Path) -> (Project, TrackId) {
    std::fs::create_dir_all(root.join("audio")).unwrap();
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut wav = hound::WavWriter::create(root.join("audio/impulse.wav"), spec).unwrap();
    for frame in 0..1000_u64 {
        let value = if frame == IMPULSE_AT { 1.0 } else { 0.0 };
        wav.write_sample(value).unwrap();
        wav.write_sample(value).unwrap();
    }
    wav.finalize().unwrap();

    let mut editor = Editor::new(Project::new("record"));
    let media = editor.alloc_id().unwrap();
    editor
        .apply(EditCommand::AddMedia {
            item: MediaItem {
                id: media,
                name: "impulse".into(),
                path: "audio/impulse.wav".into(),
                hash: "00".into(),
                sample_rate: RATE,
                channels: 2,
                frames: 1000,
                tempo: None,
                key: None,
            },
        })
        .unwrap();
    let player = add_track(&mut editor, "player");
    let armed = add_track(&mut editor, "armed");
    let clip = editor.alloc_id().unwrap();
    editor
        .apply(EditCommand::AddClip {
            clip: Box::new(Clip {
                id: clip,
                track: player,
                name: "impulse".into(),
                start: 2.0,
                length: 0.1,
                muted: false,
                content: ClipContent::Audio {
                    media,
                    source_offset: 0,
                    gain_db: 0.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                    warp: None,
                },
                take: None,
            }),
        })
        .unwrap();
    (editor.project().clone(), armed)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn read_take(path: &Path) -> (hound::WavSpec, Vec<f32>) {
    let mut reader = hound::WavReader::open(path).unwrap();
    let spec = reader.spec();
    (spec, reader.samples::<f32>().map(Result::unwrap).collect())
}

/// Records from `from_beats` until the impulse and a little more have passed, and checks the take.
fn record_and_check(input_channels: u16, from_beats: f64) {
    let work = tempfile::tempdir().unwrap();
    let (engine, _backend) = engine(input_channels);
    let (project, armed) = project(work.path());
    engine
        .set_project(project, CompileContext::new(work.path()))
        .unwrap();
    assert!(engine.send_transport(TransportCommand::LocateBeats(from_beats)));
    let from = (from_beats * BEAT as f64) as u64;
    wait_until("the locate", || engine.telemetry_bank().position() == from);

    let take_path: PathBuf = work.path().join("take.wav");
    let recording = engine
        .start_recording(&[(armed, take_path.clone())])
        .unwrap();
    let shared = Arc::clone(recording.shared());
    wait_until("the impulse to pass", || {
        engine.telemetry_bank().position() > IMPULSE_POSITION + u64::from(LATENCY) + 4_000
    });
    let stopped = engine.stop_recording(recording).unwrap();

    assert!(stopped.complete, "the audio thread confirmed the stop");
    let [take] = stopped.takes.as_slice() else {
        panic!("one take expected: {:?}", stopped.takes);
    };
    assert_eq!(take.track, armed);
    assert_eq!(take.sample_rate, RATE);
    assert_eq!(take.channels, input_channels.clamp(1, 2));
    assert_eq!(take.dropped_frames, 0);
    // The recording began at `from` (the position was settled), so the take starts one round
    // trip earlier, or at the timeline start.
    assert_eq!(take.start_sample, from.saturating_sub(u64::from(LATENCY)));
    // Every frame the tap pushed is in the file: no missing tail.
    assert!(shared.done());
    assert_eq!(take.frames, shared.captured());
    assert!(take.frames > IMPULSE_POSITION - from);

    let (spec, samples) = read_take(&take.path);
    assert_eq!(spec.channels, take.channels);
    assert_eq!(spec.sample_rate, RATE);
    assert_eq!(samples.len() as u64, take.frames * u64::from(take.channels));
    let channels = usize::from(take.channels);
    let index = samples
        .chunks(channels)
        .position(|frame| frame[0].abs() > 0.1)
        .expect("the impulse was recorded") as u64;
    let heard_at = take.start_sample + index;
    assert!(
        heard_at.abs_diff(IMPULSE_POSITION) <= 1,
        "the impulse was played at {IMPULSE_POSITION} and recorded at {heard_at}"
    );
    let impulses = samples
        .chunks(channels)
        .filter(|frame| frame[0].abs() > 0.1)
        .count();
    assert_eq!(impulses, 1, "exactly one impulse in the take");
    if channels == 2 {
        let frame = &samples[index as usize * 2..index as usize * 2 + 2];
        assert!(frame[1].abs() > 0.1, "the right channel carries it too");
    }
}

#[test]
fn a_stereo_take_places_the_impulse_within_one_sample_after_latency_compensation() {
    record_and_check(2, 1.0);
}

#[test]
fn a_take_that_starts_before_the_timeline_start_drops_the_leading_frames() {
    // Recording from position 0 with 480 frames of latency: the clip starts at 0 and the take's
    // first 480 frames (which belong before it) are gone.
    record_and_check(2, 0.0);
}

#[test]
fn a_mono_input_records_a_mono_take() {
    record_and_check(1, 1.0);
}

#[test]
fn a_wide_input_records_its_first_two_channels() {
    record_and_check(4, 1.0);
}

#[test]
fn recording_needs_an_input_and_at_least_one_track() {
    let work = tempfile::tempdir().unwrap();
    let (engine, _backend) = engine(0);
    let (project, armed) = project(work.path());
    engine
        .set_project(project, CompileContext::new(work.path()))
        .unwrap();
    assert_eq!(
        engine
            .start_recording(&[(armed, work.path().join("a.wav"))])
            .err(),
        Some(RecordError::NoInput)
    );
    let (with_input, _backend) = self::engine(2);
    assert_eq!(
        with_input.start_recording(&[]).err(),
        Some(RecordError::NoTakes)
    );
}

#[test]
fn a_recording_stopped_at_once_leaves_an_empty_take_and_a_stopped_transport() {
    let work = tempfile::tempdir().unwrap();
    let (engine, _backend) = engine(2);
    let (project, armed) = project(work.path());
    engine
        .set_project(project, CompileContext::new(work.path()))
        .unwrap();
    let recording = engine
        .start_recording(&[(armed, work.path().join("t.wav"))])
        .unwrap();
    let stopped = engine.stop_recording(recording).unwrap();
    assert!(stopped.complete);
    // Whatever was captured before the stop landed is in the file, and only that.
    assert_eq!(stopped.takes[0].dropped_frames, 0);
    let (_, samples) = read_take(&stopped.takes[0].path);
    assert_eq!(samples.len() as u64, stopped.takes[0].frames * 2);
    // The transport stopped: the position holds.
    let held = engine.telemetry_bank().position();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(engine.telemetry_bank().position(), held);
}

// ---------------------------------------------------------------------------------------------
// Punch, loop takes, count-in, monitoring and the sustained run. These drive the mock device by
// hand (`MockHandle::run_callbacks`) with injected input, so every position is exact: the input
// is a ramp whose value is its own frame number, and a take's samples say which input frames
// landed in it.
// ---------------------------------------------------------------------------------------------

use ow_engine::{ArmedTracks, MAX_LOOP_TAKES, RecordPlan, Recording, Stopped, TrackTakes};
use ow_io::mock::MockHandle;
use ow_model::{InputChannels, MonitorMode, TrackInput};

/// An engine on a hand-driven mock device with `input_channels` inputs and no loopback (the
/// input is what the test injects, the round-trip latency is 0), keeping `capture_frames` of
/// the output for [`MockHandle::captured`].
fn manual(input_channels: u16, capture_frames: usize) -> (Engine, MockHandle) {
    let backend = MockBackend::new(MockConfig {
        drive: MockDrive::Manual,
        buffer_frames: 64,
        input_channels,
        capture_frames,
        ..MockConfig::default()
    });
    let handle = backend.handle();
    let registry = BackendRegistry::with(vec![Arc::new(backend) as Arc<dyn AudioBackend>]);
    let config = EngineConfig {
        audio: AudioSelection {
            backend: "mock".into(),
            request: StreamRequest {
                input_device: (input_channels > 0).then(|| "default".into()),
                ..StreamRequest::default()
            },
        },
        ..EngineConfig::default()
    };
    let midi: Arc<dyn MidiBackend> = Arc::new(MockMidiBackend::new([] as [&str; 0]));
    (Engine::with_backends(registry, midi, config), handle)
}

/// Adds an audio track with the given input routing and monitor mode.
fn add_routed_track(
    editor: &mut Editor,
    name: &str,
    input: Option<TrackInput>,
    monitor: MonitorMode,
) -> TrackId {
    let id: TrackId = editor.alloc_id().unwrap();
    let index = editor.project().tracks.len();
    editor
        .apply(EditCommand::InsertTrack {
            index,
            bundle: Box::new(Owned::new(Track {
                id,
                name: name.into(),
                color: 0,
                kind: TrackKind::Audio,
                strip: Strip::default(),
                instrument: None,
                input,
                monitor,
            })),
        })
        .unwrap();
    id
}

/// `frames` interleaved frames of `channels` channels starting at input frame `from`; every
/// channel holds `frame * 1e-6`.
fn ramp(from: u64, frames: u64, channels: usize) -> Vec<f32> {
    (from..from + frames)
        .flat_map(|frame| std::iter::repeat_n(frame as f32 * 1e-6, channels))
        .collect()
}

/// The input frame number a ramp sample stands for.
fn frame_of(sample: f32) -> u64 {
    (sample * 1e6).round() as u64
}

/// Stops the recording while driving the device: `stop_recording` waits for the audio thread,
/// which only runs when callbacks do. The transport is stopped first, in a callback of its own,
/// so no frame is recorded between the last callback the test asked for and the stop.
fn stop_manual(engine: &Engine, handle: &MockHandle, recording: Recording) -> Stopped {
    assert!(engine.send_transport(TransportCommand::Stop));
    handle.run_callbacks(1).unwrap();
    std::thread::scope(|scope| {
        let stopper = scope.spawn(|| engine.stop_recording(recording).unwrap());
        while !stopper.is_finished() {
            handle.run_callbacks(1).unwrap();
            std::thread::yield_now();
        }
        stopper.join().unwrap()
    })
}

/// Starts the recording `plan` and runs `callbacks` callbacks over a ramp that begins at input
/// frame 0 with the first callback after the start.
fn record_ramp(
    engine: &Engine,
    handle: &MockHandle,
    plan: &RecordPlan,
    callbacks: usize,
) -> Stopped {
    let recording = engine.start_recording_plan(plan).unwrap();
    handle.inject_input(&ramp(0, callbacks as u64 * 64, 1));
    handle.run_callbacks(callbacks).unwrap();
    stop_manual(engine, handle, recording)
}

fn set_project(engine: &Engine, editor: &Editor, work: &Path) {
    engine
        .set_project(editor.project().clone(), CompileContext::new(work))
        .unwrap();
}

/// A plan of one track on the default input with `paths` takes and nothing else set.
fn plan_for(track: TrackId, paths: Vec<PathBuf>) -> RecordPlan {
    RecordPlan {
        tracks: vec![TrackTakes {
            track,
            input: None,
            paths,
        }],
        midi_tracks: Vec::new(),
        loop_beats: None,
        punch_beats: None,
        count_in_bars: 0,
    }
}

fn pass_paths(work: &Path, count: usize) -> Vec<PathBuf> {
    (0..count)
        .map(|pass| work.join(format!("pass-{pass}.wav")))
        .collect()
}

#[test]
fn punch_records_only_inside_the_range_and_places_it_at_punch_in() {
    let work = tempfile::tempdir().unwrap();
    let (engine, handle) = manual(1, 0);
    let mut editor = Editor::new(Project::new("punch"));
    let track = add_routed_track(&mut editor, "vox", None, MonitorMode::Off);
    set_project(&engine, &editor, work.path());
    // Punch from beat 1 to beat 1.5: samples 24000 to 36000 (24000 = 375 * 64 but
    // 36000 = 562.5 * 64, so punch-out falls inside a callback).
    let mut plan = plan_for(track, vec![work.path().join("punch.wav")]);
    plan.punch_beats = Some((1.0, 1.5));
    let stopped = record_ramp(&engine, &handle, &plan, 700);
    assert!(stopped.complete);
    let [take] = stopped.takes.as_slice() else {
        panic!("one take expected: {:?}", stopped.takes);
    };
    assert_eq!(take.start_sample, 24_000, "the take starts at punch-in");
    assert_eq!(take.frames, 12_000, "only the punch range is recorded");
    assert_eq!(take.dropped_frames, 0);
    let (_, samples) = read_take(&take.path);
    assert_eq!(samples.len(), 12_000);
    // The mock's latency is 0, so the input frame number is the timeline position.
    assert_eq!(frame_of(samples[0]), 24_000);
    assert_eq!(frame_of(samples[11_999]), 35_999);
    assert!(
        samples
            .windows(2)
            .all(|pair| frame_of(pair[1]) == frame_of(pair[0]) + 1),
        "no gap or repeat inside the take"
    );
    // The transport is back to a plain stop: no punch left behind for the next recording.
    assert_eq!(engine.telemetry_bank().transport_flags(), 0);
}

#[test]
fn punch_placement_follows_the_loopback_latency() {
    // A punch range around the impulse as it is heard: the take starts at punch-in minus the
    // latency, so the impulse lands where it was played.
    let work = tempfile::tempdir().unwrap();
    let (engine, _backend) = engine(2);
    let (project, armed) = project(work.path());
    engine
        .set_project(project, CompileContext::new(work.path()))
        .unwrap();
    let heard = IMPULSE_POSITION + u64::from(LATENCY);
    let punch_in = heard - 1_000;
    let punch_out = heard + 2_000;
    assert!(engine.send_transport(TransportCommand::LocateBeats(1.0)));
    wait_until("the locate", || engine.telemetry_bank().position() == BEAT);
    let mut plan = plan_for(armed, vec![work.path().join("t.wav")]);
    plan.punch_beats = Some((
        punch_in as f64 / BEAT as f64,
        punch_out as f64 / BEAT as f64,
    ));
    let recording = engine.start_recording_plan(&plan).unwrap();
    wait_until("the punch-out to pass", || {
        engine.telemetry_bank().position() > punch_out + 2_000
    });
    let stopped = engine.stop_recording(recording).unwrap();
    let [take] = stopped.takes.as_slice() else {
        panic!("one take expected: {:?}", stopped.takes);
    };
    assert_eq!(take.frames, 3_000);
    assert_eq!(take.start_sample, punch_in - u64::from(LATENCY));
    let (_, samples) = read_take(&take.path);
    let index = samples
        .chunks(2)
        .position(|frame| frame[0].abs() > 0.1)
        .expect("the impulse is inside the punch range");
    let heard_at = take.start_sample + index as u64;
    assert!(
        heard_at.abs_diff(IMPULSE_POSITION) <= 1,
        "played at {IMPULSE_POSITION}, recorded at {heard_at}"
    );
}

#[test]
fn each_loop_pass_is_a_take_with_sample_exact_boundaries() {
    let work = tempfile::tempdir().unwrap();
    let (engine, handle) = manual(1, 0);
    let mut editor = Editor::new(Project::new("loop"));
    let track = add_routed_track(&mut editor, "vox", None, MonitorMode::Off);
    set_project(&engine, &editor, work.path());
    // A half-beat loop (12000 frames, so wraps fall inside callbacks), recording from its
    // middle: the first pass is the half that is left.
    assert!(engine.send_transport(TransportCommand::LocateBeats(0.25)));
    handle.run_callbacks(1).unwrap();
    assert_eq!(engine.telemetry_bank().position(), 6_000);
    let mut plan = plan_for(track, pass_paths(work.path(), MAX_LOOP_TAKES));
    plan.loop_beats = Some((0.0, 0.5));
    // 6000 + 2 * 12000 frames fill passes 0 to 2; 469 callbacks run 30016 frames, 16 of them
    // are in a fourth pass.
    let stopped = record_ramp(&engine, &handle, &plan, 469);
    assert!(stopped.complete);
    assert_eq!(stopped.dropped_passes, 0);
    let takes: Vec<_> = stopped.takes.iter().filter(|t| t.frames > 0).collect();
    assert_eq!(
        takes.iter().map(|t| t.pass).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(
        takes.iter().map(|t| t.frames).collect::<Vec<_>>(),
        [6_000, 12_000, 12_000, 16]
    );
    // The first pass starts where recording began, the others at the loop start.
    assert_eq!(
        takes.iter().map(|t| t.start_sample).collect::<Vec<_>>(),
        [6_000, 0, 0, 0]
    );
    // Pass boundaries are the exact wrap frames: each take holds the input frames right after
    // the ones the previous take ended with.
    let mut next = 0;
    for take in &takes {
        let (_, samples) = read_take(&take.path);
        assert_eq!(samples.len() as u64, take.frames);
        assert_eq!(frame_of(samples[0]), next, "pass {} starts", take.pass);
        assert!(
            samples
                .windows(2)
                .all(|pair| frame_of(pair[1]) == frame_of(pair[0]) + 1)
        );
        next += take.frames;
    }
    assert_eq!(next, 30_016);
    // Whole transport reset: the loop is gone after the stop.
    assert_eq!(engine.telemetry_bank().transport_flags(), 0);
}

#[test]
fn passes_beyond_the_prepared_takes_are_dropped_and_counted() {
    let work = tempfile::tempdir().unwrap();
    let (engine, handle) = manual(1, 0);
    let mut editor = Editor::new(Project::new("loop"));
    let track = add_routed_track(&mut editor, "vox", None, MonitorMode::Off);
    set_project(&engine, &editor, work.path());
    let mut plan = plan_for(track, pass_paths(work.path(), 2));
    plan.loop_beats = Some((0.0, 0.5));
    // Five passes of 12000 frames (760 callbacks run 48640 frames, four full passes and a
    // part): two takes keep the first two, the other three passes are counted.
    let stopped = record_ramp(&engine, &handle, &plan, 760);
    assert_eq!(stopped.dropped_passes, 3);
    assert_eq!(
        stopped.takes.iter().map(|t| t.frames).collect::<Vec<_>>(),
        [12_000, 12_000]
    );
}

#[test]
fn the_count_in_is_heard_and_nothing_is_recorded_during_it() {
    let work = tempfile::tempdir().unwrap();
    // A bar of count-in at 120 BPM in 4/4 is 96000 frames.
    const COUNT_IN: u64 = 96_000;
    let (engine, handle) = manual(1, 130_000);
    let mut editor = Editor::new(Project::new("count-in"));
    let track = add_routed_track(&mut editor, "vox", None, MonitorMode::Off);
    set_project(&engine, &editor, work.path());
    let mut plan = plan_for(track, vec![work.path().join("t.wav")]);
    plan.count_in_bars = 1;
    let recording = engine.start_recording_plan(&plan).unwrap();
    // Input frame k of this ramp is the k-th frame after the start; an impulse sits 200
    // frames after the count-in ends.
    let mut input = ramp(0, COUNT_IN + 4_000, 1);
    input[(COUNT_IN + 200) as usize] = 1.0;
    handle.inject_input(&input);
    handle.run_callbacks(100).unwrap();
    let flags = engine.telemetry_bank().transport_flags();
    assert_eq!(
        flags & ow_ipc::TRANSPORT_COUNTING_IN,
        ow_ipc::TRANSPORT_COUNTING_IN
    );
    assert_eq!(flags & ow_ipc::TRANSPORT_RECORDING, 0);
    assert!(!recording.shared().started(), "nothing is recorded yet");
    assert_eq!(engine.telemetry_bank().position(), 0, "the position holds");
    handle
        .run_callbacks(((COUNT_IN + 4_000) / 64) as usize - 100)
        .unwrap();
    let flags = engine.telemetry_bank().transport_flags();
    assert_eq!(flags & ow_ipc::TRANSPORT_COUNTING_IN, 0);
    assert_eq!(
        flags & ow_ipc::TRANSPORT_RECORDING,
        ow_ipc::TRANSPORT_RECORDING
    );
    let stopped = stop_manual(&engine, &handle, recording);
    assert!(stopped.complete);
    let [take] = stopped.takes.as_slice() else {
        panic!("one take expected");
    };
    assert_eq!(
        take.start_sample, 0,
        "recording starts exactly at the start position"
    );
    let (_, samples) = read_take(&take.path);
    assert_eq!(
        frame_of(samples[0]),
        COUNT_IN,
        "the count-in's input is not in the take"
    );
    let index = samples.iter().position(|s| *s > 0.5).unwrap();
    assert_eq!(index, 200, "the impulse lands where it was played");
    // The click is audible during the count-in and absent from the recording that follows.
    let captured = handle.captured();
    let end = captured.len() / 2;
    let loud = |from: usize, to: usize| {
        captured[from * 2..to * 2]
            .iter()
            .fold(0.0_f32, |peak, s| peak.max(s.abs()))
    };
    assert!(loud(0, COUNT_IN as usize) > 0.1, "the count-in clicks");
    assert_eq!(
        loud(COUNT_IN as usize + 1_000, end),
        0.0,
        "no click after it"
    );
}

#[test]
fn monitoring_puts_the_input_in_the_track_output_only_while_active() {
    let work = tempfile::tempdir().unwrap();
    let (engine, handle) = manual(2, 64 * 400);
    let mut editor = Editor::new(Project::new("monitor"));
    let track = add_routed_track(&mut editor, "vox", None, MonitorMode::Off);
    let armed = ArmedTracks::default();
    let context = || {
        let mut ctx = CompileContext::new(work.path());
        ctx.armed = armed.clone();
        ctx
    };
    // The input: 0.5 on channel 0 and 0.25 on channel 1, always.
    let input: Vec<f32> = (0..64 * 400).flat_map(|_| [0.5, 0.25]).collect();
    handle.inject_input(&input);
    let mut callbacks = 0;
    let mut level = || {
        handle.run_callbacks(10).unwrap();
        callbacks += 10;
        let captured = handle.captured();
        // The last frame of the ten callbacks, well after any fade.
        let end = callbacks * 64 * 2;
        [captured[end - 2], captured[end - 1]]
    };

    engine
        .set_project(editor.project().clone(), context())
        .unwrap();
    assert_eq!(level(), [0.0, 0.0], "monitoring off: silence");

    editor
        .apply(EditCommand::SetTrackMonitor {
            track,
            monitor: MonitorMode::On,
        })
        .unwrap();
    engine
        .set_project(editor.project().clone(), context())
        .unwrap();
    let on = level();
    // The default routing is the pair 0 and 1, and the centre pan leaves a stereo pair alone.
    assert!(
        (on[0] - 0.5).abs() < 1e-6 && (on[1] - 0.25).abs() < 1e-6,
        "{on:?}"
    );

    // Automatic monitoring follows the arm flag, without a recompile.
    editor
        .apply(EditCommand::SetTrackMonitor {
            track,
            monitor: MonitorMode::Auto,
        })
        .unwrap();
    engine
        .set_project(editor.project().clone(), context())
        .unwrap();
    assert_eq!(level(), [0.0, 0.0], "auto, not armed: silence");
    armed.set(track, true);
    let auto = level();
    assert!((auto[0] - 0.5).abs() < 1e-6, "auto, armed: {auto:?}");
    armed.set(track, false);
    assert_eq!(level(), [0.0, 0.0], "disarmed again: silence");

    // A mono route to channel 1 plays it on both sides.
    editor
        .apply(EditCommand::SetTrackMonitor {
            track,
            monitor: MonitorMode::On,
        })
        .unwrap();
    editor
        .apply(EditCommand::SetTrackInput {
            track,
            input: Some(TrackInput {
                channels: InputChannels::Mono(1),
            }),
        })
        .unwrap();
    engine
        .set_project(editor.project().clone(), context())
        .unwrap();
    let mono = level();
    assert!(mono[0] > 0.1 && mono[1] > 0.1, "{mono:?}");
    assert!(
        (mono[0] - mono[1]).abs() < 1e-6,
        "one channel on both sides: {mono:?}"
    );
}

#[test]
fn a_track_routed_to_channels_the_device_lacks_cannot_start() {
    let work = tempfile::tempdir().unwrap();
    let (engine, _handle) = manual(2, 0);
    let mut editor = Editor::new(Project::new("route"));
    let track = add_routed_track(&mut editor, "vox", None, MonitorMode::Off);
    set_project(&engine, &editor, work.path());
    let mut plan = plan_for(track, vec![work.path().join("t.wav")]);
    plan.tracks[0].input = Some((2, 1));
    assert!(matches!(
        engine.start_recording_plan(&plan).err(),
        Some(RecordError::BadInput {
            device_channels: 2,
            ..
        })
    ));
}

#[test]
fn two_tracks_record_their_own_channels() {
    let work = tempfile::tempdir().unwrap();
    let (engine, handle) = manual(4, 0);
    let mut editor = Editor::new(Project::new("routes"));
    let left = add_routed_track(&mut editor, "a", None, MonitorMode::Off);
    let right = add_routed_track(&mut editor, "b", None, MonitorMode::Off);
    set_project(&engine, &editor, work.path());
    let mut plan = plan_for(left, vec![work.path().join("a.wav")]);
    plan.tracks[0].input = Some((3, 1));
    plan.tracks.push(TrackTakes {
        track: right,
        input: Some((1, 2)),
        paths: vec![work.path().join("b.wav")],
    });
    let recording = engine.start_recording_plan(&plan).unwrap();
    // Channel c holds 0.1 * (c + 1).
    let input: Vec<f32> = (0..64 * 10).flat_map(|_| [0.1, 0.2, 0.3, 0.4]).collect();
    handle.inject_input(&input);
    handle.run_callbacks(10).unwrap();
    let stopped = stop_manual(&engine, &handle, recording);
    assert_eq!(stopped.takes.len(), 2);
    let (spec, a) = read_take(&stopped.takes[0].path);
    assert_eq!(spec.channels, 1);
    assert!(a.iter().all(|s| (s - 0.4).abs() < 1e-6));
    let (spec, b) = read_take(&stopped.takes[1].path);
    assert_eq!(spec.channels, 2);
    assert!(
        b.chunks(2)
            .all(|f| (f[0] - 0.2).abs() < 1e-6 && (f[1] - 0.3).abs() < 1e-6)
    );
}

/// Sixteen armed input tracks for ten simulated minutes at 64 frames with every writer thread
/// running, as fast as the machine allows: no take may lose a frame to a full ring. Run with
/// `cargo test -p ow-engine --test record_loopback sustained --release` (debug builds skip it:
/// 450000 callbacks and 1.8 GB of WAV are release-sized work).
#[test]
#[cfg_attr(debug_assertions, ignore = "run with --release")]
fn sustained_recording_of_16_tracks_for_10_minutes_overruns_no_ring() {
    const TRACKS: usize = 16;
    const SECONDS: u64 = 600;
    const FRAMES: u64 = 64;
    const SECOND_CALLBACKS: usize = (RATE as u64 / FRAMES) as usize;
    let work = tempfile::tempdir().unwrap();
    let (engine, handle) = manual(TRACKS as u16, 0);
    let mut editor = Editor::new(Project::new("sustained"));
    let tracks: Vec<TrackId> = (0..TRACKS)
        .map(|i| {
            add_routed_track(
                &mut editor,
                &format!("in {i}"),
                Some(TrackInput {
                    channels: InputChannels::Mono(i as u16),
                }),
                MonitorMode::Off,
            )
        })
        .collect();
    set_project(&engine, &editor, work.path());
    let plan = RecordPlan {
        tracks: tracks
            .iter()
            .enumerate()
            .map(|(i, track)| TrackTakes {
                track: *track,
                input: Some((i as u16, 1)),
                paths: vec![work.path().join(format!("take-{i}.wav"))],
            })
            .collect(),
        midi_tracks: Vec::new(),
        loop_beats: None,
        punch_beats: None,
        count_in_bars: 0,
    };
    let started = Instant::now();
    let recording = engine.start_recording_plan(&plan).unwrap();
    let shared = Arc::clone(recording.shared());
    // Channel c holds a constant 0.01 * (c + 1) so a take shows which channel it recorded.
    let second: Vec<f32> = (0..SECOND_CALLBACKS as u64 * FRAMES)
        .flat_map(|_| (0..TRACKS).map(|c| 0.01 * (c + 1) as f32))
        .collect();
    let mut worst_lag = 0;
    for second_index in 0..SECONDS {
        handle.inject_input(&second);
        handle.run_callbacks(SECOND_CALLBACKS).unwrap();
        // Keep the producer from outrunning the disks by more than half a ring: wait for the
        // slowest take file to be within a second of what was captured.
        let captured = shared.captured();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let lag = (0..TRACKS)
                .map(|i| {
                    let bytes = std::fs::metadata(work.path().join(format!("take-{i}.wav")))
                        .map_or(0, |m| m.len());
                    captured.saturating_sub(bytes.saturating_sub(44) / 4)
                })
                .max()
                .unwrap_or(0);
            worst_lag = worst_lag.max(lag);
            if lag <= u64::from(RATE) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the writers stalled at second {second_index}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let stopped = stop_manual(&engine, &handle, recording);
    eprintln!(
        "sustained: {SECONDS} simulated seconds of {TRACKS} tracks in {:?}, worst writer lag {worst_lag} frames",
        started.elapsed()
    );
    assert!(stopped.complete, "no input was skipped");
    assert!(stopped.failures.is_empty(), "{:?}", stopped.failures);
    assert_eq!(stopped.takes.len(), TRACKS);
    let expected = SECONDS * u64::from(RATE);
    for (i, take) in stopped.takes.iter().enumerate() {
        assert_eq!(take.dropped_frames, 0, "ring overrun on take {i}");
        assert_eq!(take.frames, expected, "take {i} is whole");
        assert_eq!(take.channels, 1);
    }
    // Spot-check the ends of the first and last take: the right channel, no tail missing.
    for take in [&stopped.takes[0], &stopped.takes[TRACKS - 1]] {
        let channel = stopped
            .takes
            .iter()
            .position(|t| t.path == take.path)
            .unwrap();
        let reader = hound::WavReader::open(&take.path).unwrap();
        assert_eq!(u64::from(reader.duration()), expected);
        let mut samples = reader.into_samples::<f32>();
        let first = samples.next().unwrap().unwrap();
        let last = samples.last().unwrap().unwrap();
        let want = 0.01 * (channel + 1) as f32;
        assert!((first - want).abs() < 1e-6 && (last - want).abs() < 1e-6);
    }
}

#[test]
fn count_in_punch_and_loop_together() {
    let work = tempfile::tempdir().unwrap();
    // A bar of count-in is 96000 frames; the loop is 12000 frames and the punch covers
    // positions 2400 to 9600 of it.
    const COUNT_IN: u64 = 96_000;
    let (engine, handle) = manual(1, 0);
    let mut editor = Editor::new(Project::new("combined"));
    let track = add_routed_track(&mut editor, "vox", None, MonitorMode::Off);
    set_project(&engine, &editor, work.path());
    let mut plan = plan_for(track, pass_paths(work.path(), 4));
    plan.count_in_bars = 1;
    plan.loop_beats = Some((0.0, 0.5));
    plan.punch_beats = Some((0.1, 0.4));
    // The count-in and three passes, plus 32 frames of a fourth that is before the punch-in.
    let stopped = record_ramp(&engine, &handle, &plan, 2063);
    assert!(stopped.complete);
    let takes: Vec<_> = stopped.takes.iter().filter(|t| t.frames > 0).collect();
    assert_eq!(takes.len(), 3);
    for (pass, take) in takes.iter().enumerate() {
        assert_eq!(take.frames, 7_200, "pass {pass}");
        assert_eq!(take.start_sample, 2_400, "pass {pass}");
        let (_, samples) = read_take(&take.path);
        assert_eq!(
            frame_of(samples[0]),
            COUNT_IN + pass as u64 * 12_000 + 2_400,
            "pass {pass} starts at its punch-in"
        );
    }
}
