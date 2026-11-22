//! Mock backend behaviour (loopback, injected input, thread drive) and the supervisor's device
//! loss recovery and live reconfiguration, all without audio hardware.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ow_io::mock::{MockBackend, MockConfig, MockDrive};
use ow_io::{
    AudioBackend, AudioCallback, AudioSelection, AudioState, BackendRegistry, IoErrorKind, Opener,
    RecoveryPolicy, StreamHealth, StreamInfo, StreamRequest, Supervisor,
};

fn registry(backend: &MockBackend) -> BackendRegistry {
    BackendRegistry::with(vec![Arc::new(backend.clone()) as Arc<dyn AudioBackend>])
}

fn selection() -> AudioSelection {
    AudioSelection {
        backend: "mock".into(),
        request: StreamRequest::default(),
    }
}

/// Opener that opens the mock device with a callback running `make(info)`.
fn opener(
    backend: &MockBackend,
    make: impl Fn(&StreamInfo) -> AudioCallback + Send + Sync + Clone + 'static,
) -> Opener {
    let registry = registry(backend);
    Box::new(move |selection| {
        let make = make.clone();
        registry.open(selection, Box::new(move |info| make(info)))
    })
}

fn silent(_: &StreamInfo) -> AudioCallback {
    Box::new(|out, _| out.fill(0.0))
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn fast_policy() -> RecoveryPolicy {
    RecoveryPolicy {
        max_attempts: 5,
        initial_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        attempts_before_fallback: 2,
        poll_interval: Duration::from_millis(5),
    }
}

#[test]
fn mock_loopback_returns_the_output_after_the_set_latency() {
    const LATENCY: usize = 192;
    const BLOCK: usize = 64;
    let backend = MockBackend::new(MockConfig {
        input_channels: 2,
        buffer_frames: BLOCK as u32,
        loopback_latency_frames: Some(LATENCY as u32),
        ..MockConfig::default()
    });
    let heard = Arc::new(Mutex::new(Vec::<f32>::new()));
    let frame = Arc::new(AtomicU64::new(0));
    let (heard_cb, frame_cb) = (Arc::clone(&heard), Arc::clone(&frame));
    let registry = registry(&backend);
    let mut stream = registry
        .open(
            &selection(),
            Box::new(move |_| -> AudioCallback {
                Box::new(move |out, input| {
                    let input = input.expect("a loopback device has an input");
                    heard_cb
                        .lock()
                        .unwrap()
                        .extend(input.chunks(2).map(|f| f[0]));
                    for pair in out.chunks_mut(2) {
                        // A ramp 1, 2, 3, ... on the left channel.
                        pair[0] = (frame_cb.fetch_add(1, Ordering::Relaxed) + 1) as f32;
                        pair[1] = 0.0;
                    }
                })
            }),
        )
        .unwrap();
    stream.start().unwrap();
    backend.handle().run_callbacks(20).unwrap();
    let heard = heard.lock().unwrap();
    assert_eq!(heard.len(), 20 * BLOCK);
    for (index, sample) in heard.iter().enumerate() {
        let expected = if index < LATENCY {
            0.0
        } else {
            (index - LATENCY + 1) as f32
        };
        assert_eq!(*sample, expected, "frame {index}");
    }
}

#[test]
fn mock_loopback_latency_is_at_least_one_buffer() {
    let backend = MockBackend::new(MockConfig {
        input_channels: 1,
        output_channels: 1,
        buffer_frames: 32,
        loopback_latency_frames: Some(1),
        ..MockConfig::default()
    });
    let heard = Arc::new(Mutex::new(Vec::<f32>::new()));
    let heard_cb = Arc::clone(&heard);
    let mut stream = registry(&backend)
        .open(
            &selection(),
            Box::new(move |_| -> AudioCallback {
                let mut n = 0.0_f32;
                Box::new(move |out, input| {
                    heard_cb.lock().unwrap().extend_from_slice(input.unwrap());
                    for sample in out.iter_mut() {
                        n += 1.0;
                        *sample = n;
                    }
                })
            }),
        )
        .unwrap();
    stream.start().unwrap();
    backend.handle().run_callbacks(3).unwrap();
    let heard = heard.lock().unwrap();
    assert!(heard[..32].iter().all(|s| *s == 0.0));
    assert_eq!(heard[32], 1.0, "one buffer of latency, the minimum");
}

#[test]
fn mock_injected_input_reaches_the_callback_block_by_block() {
    let backend = MockBackend::new(MockConfig {
        input_channels: 1,
        output_channels: 1,
        buffer_frames: 4,
        capture_frames: 12,
        ..MockConfig::default()
    });
    let handle = backend.handle();
    // The callback echoes its input to the output.
    let mut stream = registry(&backend)
        .open(
            &selection(),
            Box::new(|_| -> AudioCallback {
                Box::new(|out, input| out.copy_from_slice(input.unwrap()))
            }),
        )
        .unwrap();
    stream.start().unwrap();
    handle.inject_input(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    handle.run_callbacks(3).unwrap();
    assert_eq!(
        handle.captured(),
        [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
    );
}

#[test]
fn mock_thread_drive_runs_callbacks_while_started_and_stops_on_stop() {
    let backend = MockBackend::new(MockConfig {
        drive: MockDrive::Thread { paced: false },
        ..MockConfig::default()
    });
    let handle = backend.handle();
    let mut stream = registry(&backend)
        .open(&selection(), Box::new(silent))
        .unwrap();
    assert_eq!(handle.callbacks_run(), 0, "nothing runs before start");
    stream.start().unwrap();
    wait_until("callbacks", || handle.callbacks_run() > 100);
    assert_eq!(stream.health(), StreamHealth::Running);
    stream.stop().unwrap();
    // Let an in-flight callback finish, then the count must hold still.
    thread::sleep(Duration::from_millis(50));
    let after_stop = handle.callbacks_run();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(handle.callbacks_run(), after_stop);
    assert_eq!(stream.health(), StreamHealth::Stopped);
}

#[test]
fn mock_rejects_unknown_devices_and_reports_the_injected_failures() {
    let backend = MockBackend::new(MockConfig::default());
    let registry = registry(&backend);
    let bad = AudioSelection {
        backend: "mock".into(),
        request: StreamRequest {
            output_device: Some("no-such-device".into()),
            ..StreamRequest::default()
        },
    };
    let error = registry.open(&bad, Box::new(silent)).err().unwrap();
    assert_eq!(error.kind(), IoErrorKind::NotFound);
    backend.handle().fail_next_opens(1);
    let error = registry.open(&selection(), Box::new(silent)).err().unwrap();
    assert_eq!(error.kind(), IoErrorKind::DeviceLost);
    assert!(registry.open(&selection(), Box::new(silent)).is_ok());
}

#[test]
fn mock_device_loss_is_recovered_by_the_supervisor() {
    let backend = MockBackend::new(MockConfig {
        drive: MockDrive::Thread { paced: false },
        ..MockConfig::default()
    });
    let handle = backend.handle();
    let supervisor = Supervisor::start(opener(&backend, silent), selection(), fast_policy());
    assert_eq!(supervisor.status().state, AudioState::Running);
    assert_eq!(handle.opens(), 1);

    handle.disconnect();
    wait_until("the stream to be reopened", || {
        let status = supervisor.status();
        status.state == AudioState::Running && status.recoveries == 1
    });
    assert_eq!(handle.opens(), 2);
    let status = supervisor.status();
    assert!(status.error.is_none());
    assert!(!status.on_fallback_device);
    // The reopened stream really runs.
    let before = handle.callbacks_run();
    wait_until("callbacks on the new stream", || {
        handle.callbacks_run() > before + 50
    });
}

#[test]
fn mock_recovery_retries_with_a_bound_then_reports_failure_and_can_be_retried() {
    let backend = MockBackend::new(MockConfig::default());
    let handle = backend.handle();
    let supervisor = Supervisor::start(opener(&backend, silent), selection(), fast_policy());
    handle.fail_next_opens(1_000);
    handle.disconnect();
    wait_until("recovery to give up", || {
        supervisor.status().state == AudioState::Failed
    });
    let status = supervisor.status();
    assert!(status.info.is_none());
    assert!(
        status
            .error
            .as_deref()
            .unwrap()
            .contains("injected open failure")
    );
    // At most `max_attempts` reopen attempts per failure (plus the initial open).
    assert!(
        handle.opens() <= 1,
        "failed opens do not count as opened streams"
    );

    // The device comes back; the user presses retry.
    handle.fail_next_opens(0);
    supervisor.retry();
    wait_until("the retry to succeed", || {
        supervisor.status().state == AudioState::Running
    });
    assert_eq!(supervisor.status().recoveries, 1);
}

#[test]
fn mock_failed_first_open_recovers_when_the_device_appears() {
    let backend = MockBackend::new(MockConfig::default());
    backend.handle().fail_next_opens(2);
    let supervisor = Supervisor::start(opener(&backend, silent), selection(), fast_policy());
    assert_eq!(supervisor.status().state, AudioState::Failed);
    wait_until("the device to open", || {
        supervisor.status().state == AudioState::Running
    });
    assert!(supervisor.status().error.is_none());
}

#[test]
fn mock_reconfigure_switches_the_stream_and_restores_on_failure() {
    let backend = MockBackend::new(MockConfig::default());
    let handle = backend.handle();
    let supervisor = Supervisor::start(opener(&backend, silent), selection(), fast_policy());
    assert_eq!(handle.opens(), 1);

    let wanted = AudioSelection {
        backend: "mock".into(),
        request: StreamRequest {
            output_device: Some("mock-device".into()),
            buffer_frames: Some(128),
            ..StreamRequest::default()
        },
    };
    let status = supervisor.reconfigure(wanted.clone()).unwrap();
    assert_eq!(status.state, AudioState::Running);
    assert_eq!(status.selection, wanted);
    assert_eq!(handle.opens(), 2);

    // A selection that cannot open leaves the previous stream running and reports the error.
    let broken = AudioSelection {
        backend: "mock".into(),
        request: StreamRequest {
            output_device: Some("unplugged".into()),
            ..StreamRequest::default()
        },
    };
    let error = supervisor.reconfigure(broken).unwrap_err();
    assert_eq!(error.kind(), IoErrorKind::NotFound);
    let status = supervisor.status();
    assert_eq!(status.state, AudioState::Running);
    assert_eq!(status.selection, wanted);
    assert!(status.error.is_some(), "the failed switch is reported");
}

#[test]
fn mock_dropping_the_supervisor_closes_the_stream() {
    let backend = MockBackend::new(MockConfig {
        drive: MockDrive::Thread { paced: false },
        ..MockConfig::default()
    });
    let handle = backend.handle();
    let supervisor = Supervisor::start(opener(&backend, silent), selection(), fast_policy());
    wait_until("callbacks", || handle.callbacks_run() > 10);
    drop(supervisor);
    let after = handle.callbacks_run();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        handle.callbacks_run(),
        after,
        "no callbacks after the supervisor is dropped"
    );
}
