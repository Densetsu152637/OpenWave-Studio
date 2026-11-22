//! `Engine::set_project` on the mock backend: plans recompile and swap without audio hardware,
//! node identity carries over by key, and a reopened stream starts on the project's plan.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ow_engine::{Engine, EngineConfig, NodeKey};
use ow_io::mock::{MockBackend, MockConfig, MockDrive, MockMidiBackend};
use ow_io::{
    AudioBackend, AudioSelection, AudioState, BackendRegistry, MidiBackend, RecoveryPolicy,
    StreamRequest,
};
use ow_model::{EditCommand, Editor, MixerRef, Owned, Project, Strip, Track, TrackId, TrackKind};

fn engine(drive: MockDrive) -> (Engine, MockBackend) {
    let backend = MockBackend::new(MockConfig {
        drive,
        buffer_frames: 128,
        capture_frames: 48_000,
        ..MockConfig::default()
    });
    let registry = BackendRegistry::with(vec![Arc::new(backend.clone()) as Arc<dyn AudioBackend>]);
    let config = EngineConfig {
        audio: AudioSelection {
            backend: "mock".into(),
            request: StreamRequest::default(),
        },
        recovery: RecoveryPolicy {
            max_attempts: 5,
            initial_delay: Duration::from_millis(5),
            max_delay: Duration::from_millis(20),
            attempts_before_fallback: 2,
            poll_interval: Duration::from_millis(5),
        },
        midi_inputs: Vec::new(),
    };
    let midi: Arc<dyn MidiBackend> = Arc::new(MockMidiBackend::new([] as [&str; 0]));
    (Engine::with_backends(registry, midi, config), backend)
}

fn add_track(editor: &mut Editor) -> TrackId {
    let id: TrackId = editor.alloc_id().unwrap();
    let index = editor.project().tracks.len();
    editor
        .apply(EditCommand::InsertTrack {
            index,
            bundle: Box::new(Owned::new(Track {
                id,
                name: "t".into(),
                color: 0,
                kind: TrackKind::Audio,
                strip: Strip::default(),
                instrument: None,
            })),
        })
        .unwrap();
    id
}

#[test]
fn project_swap_recompiles_live_and_carries_nodes_by_key() {
    let (engine, backend) = engine(MockDrive::Manual);
    assert_eq!(engine.audio_status().state, AudioState::Running);
    let mut editor = Editor::new(Project::new("swap"));

    let first = engine.set_project(editor.project().clone()).unwrap();
    assert!(first.applied);
    assert!(
        first
            .diff
            .added
            .iter()
            .any(|(key, _)| *key == NodeKey::Fader(MixerRef::Master))
    );
    backend.handle().run_callbacks(50).unwrap();

    let track = add_track(&mut editor);
    let second = engine.set_project(editor.project().clone()).unwrap();
    assert!(second.applied);
    assert!(
        second
            .diff
            .carried
            .iter()
            .any(|c| c.key == NodeKey::Fader(MixerRef::Master)),
        "the master carries over by key"
    );
    assert!(
        second
            .diff
            .added
            .iter()
            .any(|(key, _)| *key == NodeKey::Fader(MixerRef::Track(track)))
    );
    backend.handle().run_callbacks(50).unwrap();
    // The project plan replaced the smoke-path tone.
    assert!(!engine.set_test_tone(true));
    assert!(engine.telemetry_bank().cpu_load() >= 0.0);
}

#[test]
fn project_swap_rejects_an_invalid_project_and_keeps_playing() {
    let (engine, backend) = engine(MockDrive::Manual);
    assert!(engine.set_project(Project::new("ok")).unwrap().applied);
    let mut bad = Project::new("bad");
    bad.settings.sample_rate = 0;
    assert!(engine.set_project(bad).is_err());
    backend.handle().run_callbacks(20).unwrap();
}

#[test]
fn a_reopened_stream_starts_on_the_projects_plan() {
    let (engine, backend) = engine(MockDrive::Thread { paced: false });
    assert!(
        engine.set_test_tone(true),
        "the smoke tone exists before a project"
    );
    assert!(engine.set_project(Project::new("p")).unwrap().applied);

    backend.handle().disconnect();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = engine.audio_status();
        if status.state == AudioState::Running && status.recoveries == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "timed out waiting for recovery");
        thread::sleep(Duration::from_millis(5));
    }
    // The new stream compiled the project instead of the smoke plan.
    assert!(!engine.set_test_tone(true));
    assert!(engine.set_project(Project::new("p2")).unwrap().applied);
}
