//! MVP goal M-G6 through the app's control layer, without a window: a recording on the mock
//! device (the output looped back to the input after a set latency) is written into the media
//! root, added as a clip on the armed track and its peaks are served. The take lands where the
//! impulse that was played is, to the sample, after latency compensation.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ow_engine::{Engine, EngineConfig, TransportCommand};
use ow_io::mock::{MockBackend, MockConfig, MockDrive, MockMidiBackend};
use ow_io::{AudioBackend, AudioSelection, BackendRegistry, MidiBackend, StreamRequest};
use ow_ipc::peaks::decode_peaks;
use ow_ipc::{EditCommand, UiCommand};
use ow_model::{Clip, ClipContent, MediaId, Owned, Strip, Track, TrackId, TrackKind};

use super::mvp::{Ui, command, edit};
use super::{IpcState, lock};

const RATE: u32 = 48_000;
/// One beat at the default 120 BPM.
const BEAT: u64 = 24_000;
/// Round trip of the mock device, in frames.
const LATENCY: u64 = 480;
const IMPULSE_AT: u64 = 100;
/// The impulse clip starts at beat 2.
const IMPULSE_POSITION: u64 = 2 * BEAT + IMPULSE_AT;

fn engine(input_channels: u16) -> Engine {
    let backend = MockBackend::new(MockConfig {
        drive: MockDrive::Thread { paced: true },
        buffer_frames: 64,
        input_channels,
        loopback_latency_frames: (input_channels > 0).then_some(LATENCY as u32),
        ..MockConfig::default()
    });
    let registry = BackendRegistry::with(vec![Arc::new(backend) as Arc<dyn AudioBackend>]);
    let config = EngineConfig {
        audio: AudioSelection {
            backend: "mock".into(),
            request: StreamRequest::default(),
        },
        ..EngineConfig::default()
    };
    let midi: Arc<dyn MidiBackend> = Arc::new(MockMidiBackend::new([] as [&str; 0]));
    Engine::with_backends(registry, midi, config)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A stereo file of 1000 frames that is silent except for a full-scale impulse at `IMPULSE_AT`.
fn write_impulse(path: &Path) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for frame in 0..1000_u64 {
        let value = if frame == IMPULSE_AT { 1.0 } else { 0.0 };
        writer.write_sample(value).unwrap();
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
}

fn add_track(state: &IpcState, engine: &Engine, name: &str) -> TrackId {
    let ids = command(state, engine, UiCommand::AllocIds { count: 1 });
    let id = TrackId(ids[0]);
    let index = lock(&state.control).session().project().tracks.len();
    edit(
        state,
        engine,
        EditCommand::InsertTrack {
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
        },
    );
    id
}

/// A project with a "player" track that plays the impulse at beat 2 and an empty "armed" track.
fn project_with_impulse(state: &IpcState, engine: &Engine, work: &Path) -> (TrackId, TrackId) {
    let wav = work.join("impulse.wav");
    write_impulse(&wav);
    let media = state.media_import(&wav, engine).unwrap().media;
    let player = add_track(state, engine, "player");
    let armed = add_track(state, engine, "armed");
    let ids = command(state, engine, UiCommand::AllocIds { count: 1 });
    edit(
        state,
        engine,
        EditCommand::AddClip {
            clip: Box::new(Clip {
                id: ids[0].into(),
                track: player,
                name: "impulse".into(),
                start: 2.0,
                length: 0.1,
                muted: false,
                content: ClipContent::Audio {
                    media: MediaId(media),
                    source_offset: 0,
                    gain_db: 0.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                },
                take: None,
            }),
        },
    );
    (player, armed)
}

#[test]
fn mvp_record_adds_the_take_as_a_clip_on_the_armed_track_where_the_impulse_was_played() {
    let work = tempfile::tempdir().unwrap();
    let engine = engine(2);
    let state = IpcState::new();
    let mut ui = Ui::attach(&state);
    let (player, armed) = project_with_impulse(&state, &engine, work.path());
    ui.sync(&state);

    // Arming is session state: the mirror shows it, and it is neither an edit nor a change.
    let before = ui.sync(&state).history;
    assert!(before.armed_tracks.is_empty());
    state.record_set_arm(armed.0, true).unwrap();
    let after = ui.sync(&state).history;
    assert_eq!(after.armed_tracks, [armed.0]);
    assert_eq!(
        (
            after.dirty,
            after.can_undo,
            after.undo_label.clone(),
            after.can_redo
        ),
        (
            before.dirty,
            before.can_undo,
            before.undo_label.clone(),
            before.can_redo
        ),
        "arming is not an edit"
    );
    assert!(state.record_set_arm(9_999, true).is_err(), "no such track");
    state.record_set_arm(player.0, true).unwrap();
    state.record_set_arm(player.0, false).unwrap();
    assert_eq!(ui.sync(&state).history.armed_tracks, [armed.0]);

    // Record from beat 1 until the impulse and a little more have passed.
    assert!(engine.send_transport(TransportCommand::LocateBeats(1.0)));
    wait_until("the locate", || engine.telemetry_bank().position() == BEAT);
    state.record_start(&engine).unwrap();
    assert!(state.record_start(&engine).is_err(), "already recording");
    assert!(
        state.record_set_arm(armed.0, false).is_err(),
        "arming is frozen while recording"
    );
    wait_until("the impulse to pass", || {
        engine.telemetry_bank().position() > IMPULSE_POSITION + LATENCY + 4_000
    });
    state.transport_stop(&engine).unwrap();

    // One new clip on the armed track, its media in the media root, peaks registered.
    let snapshot = ui.sync(&state);
    assert_eq!(snapshot.history.armed_tracks, [armed.0]);
    assert!(snapshot.history.dirty);
    assert!(snapshot.history.engine_error.is_none());
    assert_eq!(snapshot.project.clips.len(), 2);
    assert_eq!(snapshot.project.media_pool.len(), 2);
    let take = snapshot
        .project
        .clips
        .values()
        .find(|clip| clip.track == armed)
        .expect("a clip on the armed track");
    let ClipContent::Audio {
        media,
        source_offset,
        ..
    } = take.content
    else {
        panic!("an audio clip");
    };
    assert_eq!(source_offset, 0);
    let item = snapshot.project.media_pool[&media].clone();
    let root = lock(&state.control).media_root().to_path_buf();
    let file = root.join(&item.path);
    assert!(file.is_file(), "the take is in the media root");
    assert_eq!((item.sample_rate, item.channels), (RATE, 2));

    // The clip starts one round trip before the recording began, and runs for the whole take.
    let start = (take.start * BEAT as f64).round() as u64;
    assert_eq!(start, BEAT - LATENCY);
    let length = (take.length * BEAT as f64).round() as u64;
    assert_eq!(length, item.frames, "the clip covers every recorded frame");

    // The impulse is where it was played: within one sample after compensation.
    let mut reader = hound::WavReader::open(&file).unwrap();
    assert_eq!(u64::from(reader.duration()), item.frames);
    let samples: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
    let index = samples
        .chunks(2)
        .position(|frame| frame[0].abs() > 0.1)
        .expect("the impulse was recorded") as u64;
    let heard_at = start + index;
    assert!(
        heard_at.abs_diff(IMPULSE_POSITION) <= 1,
        "played at {IMPULSE_POSITION}, recorded at {heard_at}"
    );

    // The waveform is served.
    let bytes = state.peaks.range(media.0, 0, item.frames, 50);
    let peaks = decode_peaks(&bytes).unwrap();
    assert_eq!(peaks.len(), 50);
    assert!(peaks.iter().any(|(_, high)| *high > 0.1));

    // The transport stopped and holds its position.
    let held = engine.telemetry_bank().position();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(engine.telemetry_bank().position(), held);

    // The recording is one undo step: media and clip go together.
    command(&state, &engine, UiCommand::Undo);
    let snapshot = ui.sync(&state);
    assert_eq!(snapshot.project.clips.len(), 1);
    assert_eq!(snapshot.project.media_pool.len(), 1);
    // And the track stays armed for the next take.
    assert_eq!(snapshot.history.armed_tracks, [armed.0]);
}

#[test]
fn mvp_record_is_refused_without_an_armed_track_or_an_input() {
    let work = tempfile::tempdir().unwrap();
    let engine = engine(2);
    let state = IpcState::new();
    let mut ui = Ui::attach(&state);
    let (_, armed) = project_with_impulse(&state, &engine, work.path());

    let error = state.record_start(&engine).unwrap_err();
    assert!(error.contains("arm"), "{error}");
    // Stopping when nothing records is a plain stop.
    state.transport_stop(&engine).unwrap();

    // A stream without an input cannot record.
    let silent = self::engine(0);
    state.record_set_arm(armed.0, true).unwrap();
    let error = state.record_start(&silent).unwrap_err();
    assert!(error.contains("input"), "{error}");
    assert_eq!(ui.sync(&state).history.armed_tracks, [armed.0]);

    // A deleted track is disarmed.
    edit(&state, &engine, EditCommand::RemoveTrack { id: armed });
    assert!(ui.sync(&state).history.armed_tracks.is_empty());
}

#[test]
fn mvp_record_refuses_to_replace_the_project_and_stopping_keeps_the_take() {
    let work = tempfile::tempdir().unwrap();
    let engine = engine(2);
    let state = IpcState::new();
    let mut ui = Ui::attach(&state);
    let (_, armed) = project_with_impulse(&state, &engine, work.path());
    state.record_set_arm(armed.0, true).unwrap();
    state.record_start(&engine).unwrap();
    wait_until("some audio", || engine.telemetry_bank().position() > 2_000);

    // A new or opened project would take the takes' project away: refused, nothing changes.
    let seq = ui.sync(&state).seq;
    let error = state.project_new(&engine).unwrap_err();
    assert_eq!(error, "Stop recording before opening or creating a project");
    let error = state
        .project_open(&work.path().join("any.owave"), &engine)
        .unwrap_err();
    assert!(error.contains("Stop recording"), "{error}");
    assert_eq!(ui.sync(&state).seq, seq);

    // Stopping keeps the take as a clip on the armed track.
    state.transport_stop(&engine).unwrap();
    let snapshot = ui.sync(&state);
    assert_eq!(snapshot.project.clips.len(), 2);
    assert!(
        snapshot
            .project
            .clips
            .values()
            .any(|clip| clip.track == armed)
    );
    // Now the project can be replaced.
    state.project_new(&engine).unwrap();
}
