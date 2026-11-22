//! The engine on the mock device backend: tone through the callback, MIDI inputs on their own
//! rings, live reconfiguration and recovery after device loss, without audio hardware.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ow_engine::{Engine, EngineConfig, MIDI_RINGS, MidiInputSelection};
use ow_io::mock::{MockBackend, MockConfig, MockDrive, MockMidiBackend};
use ow_io::{
    AudioBackend, AudioSelection, AudioState, BackendRegistry, MidiBackend, RecoveryPolicy,
    StreamRequest,
};

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn engine(drive: MockDrive, midi: &MockMidiBackend) -> (Engine, MockBackend) {
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
    let midi: Arc<dyn MidiBackend> = Arc::new(midi.clone());
    (Engine::with_backends(registry, midi, config), backend)
}

#[test]
fn mock_engine_plays_the_test_tone_through_the_device_callback() {
    let midi = MockMidiBackend::new(["Keys"]);
    let (engine, backend) = engine(MockDrive::Manual, &midi);
    let status = engine.audio_status();
    assert_eq!(status.state, AudioState::Running);
    let info = status.info.unwrap();
    assert_eq!((info.sample_rate, info.buffer_frames), (48_000, Some(128)));

    assert!(engine.set_test_tone(true));
    backend.handle().run_callbacks(100).unwrap();
    let captured = backend.handle().captured();
    let peak = captured.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.15, "the tone is audible, peak = {peak}");
    assert!(engine.test_tone());
}

#[test]
fn mock_engine_midi_inputs_get_their_own_rings_and_reach_the_core() {
    let midi = MockMidiBackend::new(["Keys", "Pads", "Surface"]);
    let (engine, backend) = engine(MockDrive::Manual, &midi);
    let states = engine.midi().set_inputs(vec![
        MidiInputSelection {
            port: "Keys".into(),
            control_surface: false,
        },
        MidiInputSelection {
            port: "Pads".into(),
            control_surface: false,
        },
        MidiInputSelection {
            port: "Surface".into(),
            control_surface: true,
        },
        MidiInputSelection {
            port: "Missing".into(),
            control_surface: false,
        },
    ]);
    assert_eq!(
        states.iter().map(|s| s.connected).collect::<Vec<_>>(),
        [true, true, true, false]
    );
    assert!(states[3].error.as_deref().unwrap().contains("not found"));

    for (port, note) in [("Keys", 60), ("Pads", 36), ("Surface", 7)] {
        assert!(midi.send(port, 1, &[0x90, note, 100]));
        midi.flush(port);
    }
    backend.handle().run_callbacks(4).unwrap();
    let status = engine.midi().status();
    assert_eq!(status.events, 3);
    assert_eq!(status.dropped, 0);
    assert_eq!(status.last.map(|m| m[0]), Some(0x90));
}

#[test]
fn mock_engine_limits_private_midi_inputs_to_the_ring_pool() {
    let ports: Vec<String> = (0..MIDI_RINGS + 1).map(|i| format!("In {i}")).collect();
    let midi = MockMidiBackend::new(ports.clone());
    let (engine, _backend) = engine(MockDrive::Manual, &midi);
    let states = engine.midi().set_inputs(
        ports
            .iter()
            .map(|port| MidiInputSelection {
                port: port.clone(),
                control_surface: false,
            })
            .collect(),
    );
    assert_eq!(states.iter().filter(|s| s.connected).count(), MIDI_RINGS);
    assert!(
        states
            .last()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("at most")
    );
}

#[test]
fn mock_engine_reopens_after_device_loss_and_reconnects_midi() {
    let midi = MockMidiBackend::new(["Keys"]);
    let (engine, backend) = engine(MockDrive::Thread { paced: false }, &midi);
    engine.midi().set_inputs(vec![MidiInputSelection {
        port: "Keys".into(),
        control_surface: false,
    }]);
    assert!(engine.set_test_tone(true));

    backend.handle().disconnect();
    wait_until("recovery", || {
        let status = engine.audio_status();
        status.state == AudioState::Running && status.recoveries == 1
    });
    assert_eq!(backend.handle().opens(), 2);
    // A fresh core: the tone is off again, and MIDI is reconnected to the new rings.
    assert!(!engine.test_tone());
    wait_until("the midi input to reconnect", || {
        engine.midi().status().ports.iter().all(|p| p.connected)
    });
    assert!(midi.send("Keys", 5, &[0x90, 64, 90]));
    midi.flush("Keys");
    assert_eq!(engine.midi().status().events, 1);
    assert!(engine.set_test_tone(true));
}

#[test]
fn mock_engine_switches_buffer_size_while_running() {
    let midi = MockMidiBackend::new([] as [&str; 0]);
    let (engine, backend) = engine(MockDrive::Thread { paced: false }, &midi);
    assert!(engine.set_test_tone(true));
    wait_until("callbacks", || backend.handle().callbacks_run() > 20);
    let status = engine
        .reconfigure_audio(AudioSelection {
            backend: "mock".into(),
            request: StreamRequest {
                buffer_frames: Some(256),
                ..StreamRequest::default()
            },
        })
        .unwrap();
    assert_eq!(status.state, AudioState::Running);
    assert_eq!(backend.handle().opens(), 2);
    let before = backend.handle().callbacks_run();
    wait_until("callbacks after the switch", || {
        backend.handle().callbacks_run() > before + 20
    });
    assert!(
        engine.set_test_tone(true),
        "the new core accepts control messages"
    );
}
