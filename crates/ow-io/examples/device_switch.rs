//! Manual device check against real hardware: lists the devices of every backend, plays a quiet
//! 440 Hz tone, then switches buffer size (and, with `--exclusive`, to WASAPI exclusive mode)
//! while it plays and reports how long audio took to resume.
//!
//! `cargo run -p ow-io --example device_switch` (add `-- --exclusive` on Windows). Exit code 0
//! means every step opened and kept calling the callback; any failure prints the reason.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ow_io::{
    AudioCallback, AudioSelection, AudioState, BackendRegistry, MidiBackend, MidirBackend, Opener,
    RecoveryPolicy, StreamInfo, StreamRequest, Supervisor,
};

const AMPLITUDE: f32 = 0.05;

fn main() {
    let exclusive = std::env::args().any(|arg| arg == "--exclusive");
    let registry = BackendRegistry::system();
    for backend in registry.all() {
        println!("backend {} ({})", backend.id(), backend.label());
        match backend.devices() {
            Ok(devices) => {
                for device in devices {
                    println!(
                        "  {} out={} in={} rates={:?} buffers={:?}{}",
                        device.name,
                        device.max_output_channels,
                        device.max_input_channels,
                        device.sample_rates,
                        device.buffer_frames,
                        if device.is_default_output {
                            " [default out]"
                        } else {
                            ""
                        },
                    );
                }
            }
            Err(error) => println!("  device list failed: {error}"),
        }
    }

    let midi = MidirBackend::new();
    match (midi.inputs(), midi.outputs()) {
        (Ok(inputs), Ok(outputs)) => println!(
            "midi inputs: {:?}, outputs: {:?}",
            inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            outputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>()
        ),
        (inputs, outputs) => println!(
            "midi listing failed: {:?} {:?}",
            inputs.err(),
            outputs.err()
        ),
    }

    let callbacks = Arc::new(AtomicU64::new(0));
    let opener: Opener = {
        let registry = registry.clone();
        let callbacks = Arc::clone(&callbacks);
        Box::new(move |selection| {
            let callbacks = Arc::clone(&callbacks);
            registry.open(
                selection,
                Box::new(move |info: &StreamInfo| -> AudioCallback {
                    let channels = usize::from(info.output_channels).max(1);
                    let step = 440.0 * std::f32::consts::TAU / info.sample_rate as f32;
                    let mut phase = 0.0_f32;
                    Box::new(move |out, _| {
                        for frame in out.chunks_mut(channels) {
                            let value = phase.sin() * AMPLITUDE;
                            phase = (phase + step) % std::f32::consts::TAU;
                            frame.fill(value);
                        }
                        callbacks.fetch_add(1, Ordering::Relaxed);
                    })
                }),
            )
        })
    };

    let first = default_backend_selection(&registry, Some(256));
    println!("\nopening {first:?}");
    let supervisor = Supervisor::start(opener, first, RecoveryPolicy::default());
    report(&supervisor);
    assert_calling(&callbacks, "initial stream");

    let mut steps = vec![(
        "buffer 128",
        default_backend_selection(&registry, Some(128)),
    )];
    steps.push((
        "buffer 512",
        default_backend_selection(&registry, Some(512)),
    ));
    if exclusive {
        steps.push((
            "WASAPI exclusive, buffer 128",
            AudioSelection {
                backend: "wasapi-exclusive".into(),
                request: StreamRequest {
                    buffer_frames: Some(128),
                    ..StreamRequest::default()
                },
            },
        ));
        steps.push((
            "back to the shared host",
            default_backend_selection(&registry, Some(256)),
        ));
    }
    let mut failed = false;
    for (label, selection) in steps {
        let started = Instant::now();
        match supervisor.reconfigure(selection) {
            Ok(_) => {
                // The old stream is closed by now, so these callbacks come from the new one.
                let after = callbacks.load(Ordering::Relaxed);
                let resumed = wait_for_callbacks(&callbacks, after + 10);
                match resumed {
                    Some(at) => println!(
                        "switch to {label}: audio resumed after {} ms",
                        at.duration_since(started).as_millis()
                    ),
                    None => {
                        println!("switch to {label}: FAILED, no callbacks after 5 s");
                        failed = true;
                    }
                }
                report(&supervisor);
            }
            Err(error) => {
                println!("switch to {label}: FAILED to open: {error}");
                failed = true;
            }
        }
        thread::sleep(Duration::from_millis(400));
    }
    drop(supervisor);
    if failed {
        std::process::exit(1);
    }
    println!("\nall switches resumed audio");
}

fn default_backend_selection(registry: &BackendRegistry, buffer: Option<u32>) -> AudioSelection {
    AudioSelection {
        backend: registry.all().first().map(|b| b.id()).unwrap_or_default(),
        request: StreamRequest {
            buffer_frames: buffer,
            ..StreamRequest::default()
        },
    }
}

fn report(supervisor: &Supervisor) {
    let status = supervisor.status();
    match (&status.state, &status.info) {
        (AudioState::Running, Some(info)) => println!(
            "  running: {} via {} at {} Hz, {} ch, buffer {:?}{}",
            info.device_name,
            info.backend,
            info.sample_rate,
            info.output_channels,
            info.buffer_frames,
            if info.exclusive { ", exclusive" } else { "" },
        ),
        other => println!("  state {other:?}, error {:?}", status.error),
    }
}

fn wait_for_callbacks(counter: &AtomicU64, target: u64) -> Option<Instant> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if counter.load(Ordering::Relaxed) >= target {
            return Some(Instant::now());
        }
        thread::sleep(Duration::from_millis(1));
    }
    None
}

fn assert_calling(counter: &AtomicU64, what: &str) {
    if wait_for_callbacks(counter, 20).is_none() {
        println!("{what}: FAILED, the callback was not called");
        std::process::exit(1);
    }
    println!("{what}: callback running");
}
