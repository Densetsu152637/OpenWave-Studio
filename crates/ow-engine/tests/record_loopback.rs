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
                },
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
