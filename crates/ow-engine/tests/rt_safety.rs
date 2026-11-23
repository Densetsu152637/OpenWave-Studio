//! Real-time safety: the engine cycle must not allocate (or free) with plan swaps in flight or
//! with a producer ring overflowing, on the callback thread or on any DSP worker.
//!
//! `AllocDisabler` is installed as this test binary's global allocator, and `AudioCore::process`
//! runs every cycle under `assert_no_alloc`, which aborts the process on any allocation,
//! reallocation or deallocation inside the cycle. DSP workers run their whole loop under the
//! same guard.
//!
//! `OW_TEST_WORKERS=<n>` builds every core in this file with `n` DSP workers (default 0), so
//! `OW_TEST_WORKERS=8 cargo test -p ow-engine --test rt_safety` repeats the suite with the
//! multithreaded scheduler.

#[cfg(not(debug_assertions))]
compile_error!(
    "rt_safety needs debug assertions: assert_no_alloc is compiled out of release builds \
     (feature `disable_release`), so these tests would pass without checking anything. \
     Run them in the dev or test profile."
);

use std::cell::Cell;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use assert_no_alloc::AllocDisabler;
use ow_engine::CompileContext;
use ow_engine::graph::nodes::{GAIN_PARAM, GainNode, MasterNode, OscillatorNode};
use ow_engine::{
    CoreBuilder, Graph, MAX_BLOCK, Message, MidiMessage, Node, NodeId, NodeKey, ProcessContext,
    RenderPlan, TransportCommand, compile_project_metered,
};
use ow_ipc::{Overflow, TelemetryBank, meter_slot};
use ow_model::{
    Clip, ClipContent, EditCommand, Editor, MediaItem, MixerRef, Owned, Project, Strip, Track,
    TrackKind,
};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const SAMPLE_RATE: u32 = 48_000;

/// Worker count from `OW_TEST_WORKERS` (0 when unset).
fn test_workers() -> usize {
    std::env::var("OW_TEST_WORKERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

/// A core builder with the worker count under test.
fn builder() -> CoreBuilder {
    CoreBuilder::new().workers(test_workers())
}

/// Plans alternate between a short and a long chain so buffer counts and node counts differ
/// from swap to swap. Returns the plan and the id of its first gain node.
fn plan(variant: usize) -> (RenderPlan, NodeId) {
    let mut graph = Graph::new();
    let osc = graph.add_node(Box::new(OscillatorNode::new(220.0 + variant as f32, 0.3)));
    let first_gain = graph.add_node(Box::new(GainNode::new(1, 0.5)));
    graph.connect(osc, 0, first_gain, 0).unwrap();
    let mut tail = first_gain;
    for _ in 0..(variant % 4) {
        let gain = graph.add_node(Box::new(GainNode::new(1, 0.9)));
        graph.connect(tail, 0, gain, 0).unwrap();
        tail = gain;
    }
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(tail, 0, master, 0).unwrap();
    graph.connect(tail, 0, master, 1).unwrap();
    graph.set_master(master).unwrap();
    (graph.compile(SAMPLE_RATE, MAX_BLOCK).unwrap(), first_gain)
}

#[test]
fn no_allocation_across_1000_plan_swaps() {
    const SWAPS: u64 = 1000;
    let builder = builder().plan_queue(4).control_ring(256);
    let (mut link, mut core) = builder.build();
    let telemetry = Arc::clone(link.telemetry());
    let stop = Arc::new(AtomicBool::new(false));
    let stop_audio = Arc::clone(&stop);

    // The "audio thread": a plain thread calling the cycle in a tight loop with small buffers.
    let audio = thread::spawn(move || {
        let mut buffer = vec![0.0_f32; 64 * 2];
        let mut peak = 0.0_f32;
        while !stop_audio.load(Ordering::Relaxed) {
            core.process(&mut buffer, 2, None);
            peak = buffer.iter().fold(peak, |m, s| m.max(s.abs()));
            thread::yield_now();
        }
        peak
    });

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut sent = 0;
    let mut gain_node = NodeId(1);
    while sent < SWAPS {
        assert!(Instant::now() < deadline, "timed out after {sent} swaps");
        let (next, gain) = plan(sent as usize);
        match link.swap_plan(next) {
            Ok(()) => {
                sent += 1;
                gain_node = gain;
            }
            Err(error) => {
                drop(error.0);
                thread::yield_now();
            }
        }
        // Parameter traffic alongside the swaps; some may be unrouted mid-swap, which is fine.
        link.send(Message::SetParam {
            node: gain_node,
            param: GAIN_PARAM,
            value: 0.25,
        });
        link.collect_retired();
    }
    while telemetry.plan_swaps() < SWAPS {
        assert!(
            Instant::now() < deadline,
            "audio thread did not apply all plans"
        );
        thread::yield_now();
    }
    stop.store(true, Ordering::Relaxed);
    let peak = audio.join().expect("audio thread survived 1000 swaps");
    link.collect_retired();
    assert_eq!(telemetry.plan_swaps(), SWAPS);
    assert!(peak > 0.0, "the engine produced audio during the swaps");
}

#[test]
fn ring_overflow_counts_without_allocating_or_panicking() {
    let mut builder = builder().control_ring(8);
    let mut midi = builder.producer_ring(4);
    let (mut link, mut core) = builder.build();
    assert_eq!(core.ring_count(), 2);
    let (first, gain) = plan(1);
    link.swap_plan(first).unwrap();
    let mut buffer = vec![0.0_f32; 128 * 2];
    core.process(&mut buffer, 2, None);

    // Far more messages than either ring holds, while the audio thread is not draining.
    for i in 0..100 {
        link.send(Message::SetParam {
            node: gain,
            param: GAIN_PARAM,
            value: i as f32 / 100.0,
        });
        midi.push(Message::Midi {
            node: gain,
            message: MidiMessage {
                status: 0x90,
                data1: 60,
                data2: 100,
            },
            offset: 0,
        });
    }
    assert_eq!(link.message_overflow(), 92);
    assert_eq!(midi.overflow_count(), 96);

    for _ in 0..50 {
        core.process(&mut buffer, 2, None);
    }
    assert_eq!(core.overflow_total(), 92 + 96);

    // Still healthy and still accepting messages after the overflow.
    assert!(link.send(Message::Transport(TransportCommand::Play)));
    core.process(&mut buffer, 2, None);
    assert!(core.transport().playing);
}

#[test]
fn oversized_buffers_and_empty_cores_are_allocation_free() {
    let (mut link, mut core) = builder().build();
    // No plan yet: silence, no allocation.
    let mut big = vec![1.0_f32; (MAX_BLOCK * 2 + 17) * 2];
    core.process(&mut big, 2, None);
    assert!(big.iter().all(|s| *s == 0.0));
    // A device buffer larger than the plan block is chunked inside the cycle.
    link.swap_plan(plan(0).0).unwrap();
    core.process(&mut big, 2, None);
    assert!(big.iter().any(|s| s.abs() > 0.01));
    // Mono and wide device layouts.
    let mut mono = vec![0.0_f32; 256];
    core.process(&mut mono, 1, None);
    let mut wide = vec![0.0_f32; 256 * 6];
    core.process(&mut wide, 6, None);
}

/// A node that allocates on every block: the thing the guard exists to catch.
struct AllocatingNode;

impl Node for AllocatingNode {
    fn name(&self) -> &'static str {
        "allocating"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let scratch: Vec<u8> = Vec::with_capacity(64);
        std::hint::black_box(&scratch);
        ctx.outputs.channel_mut(0).fill(0.0);
    }
}

thread_local! {
    /// Set on the thread that calls `AudioCore::process` in the worker canary.
    static ON_CALLBACK_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Burns a little time, then allocates, but only off the callback thread (on a DSP worker).
struct AllocatingOnWorkerNode;

impl Node for AllocatingOnWorkerNode {
    fn name(&self) -> &'static str {
        "allocating-on-worker"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let out = ctx.outputs.channel_mut(0);
        let mut value = 0.0_f32;
        for sample in out.iter_mut() {
            for _ in 0..64 {
                value = std::hint::black_box(value * 0.5 + 0.25);
            }
            *sample = value;
        }
        if !ON_CALLBACK_THREAD.get() {
            let scratch: Vec<u8> = Vec::with_capacity(64);
            std::hint::black_box(&scratch);
        }
    }
}

/// Child for the worker canary: many allocating-on-worker nodes and two workers, cycled until
/// a worker picks one up (the guard must abort the process) or 20 s pass (exit 0: failure).
fn worker_canary_child() {
    let mut graph = Graph::new();
    let master = graph.add_node(Box::new(MasterNode::new()));
    for index in 0..64 {
        let node = graph.add_node(Box::new(AllocatingOnWorkerNode));
        graph.connect(node, 0, master, index % 2).unwrap();
    }
    graph.set_master(master).unwrap();
    let (mut link, mut core) = CoreBuilder::new().workers(2).build();
    link.swap_plan(graph.compile(SAMPLE_RATE, 256).unwrap())
        .unwrap();
    let mut buffer = vec![0.0_f32; 256 * 2];
    ON_CALLBACK_THREAD.set(true);
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        core.process(&mut buffer, 2, None);
    }
}

/// Child half of the canaries: does nothing unless the parent set `OW_RT_CANARY`.
/// `alloc` runs the allocating node (the process must abort); `clean` runs a normal graph
/// (the process must exit 0), proving the subprocess mechanism itself works; `worker-alloc`
/// allocates only on DSP workers (the process must abort).
#[test]
fn canary_child() {
    let Ok(mode) = std::env::var("OW_RT_CANARY") else {
        return;
    };
    if mode == "worker-alloc" {
        worker_canary_child();
        return;
    }
    let mut graph = Graph::new();
    let source = if mode == "alloc" {
        graph.add_node(Box::new(AllocatingNode))
    } else {
        graph.add_node(Box::new(OscillatorNode::new(440.0, 0.1)))
    };
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(source, 0, master, 0).unwrap();
    graph.set_master(master).unwrap();
    let (mut link, mut core) = builder().build();
    link.swap_plan(graph.compile(SAMPLE_RATE, 256).unwrap())
        .unwrap();
    let mut buffer = vec![0.0_f32; 256 * 2];
    core.process(&mut buffer, 2, None);
}

fn run_canary(mode: &str) -> std::process::Output {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "canary_child", "--nocapture", "--test-threads=1"])
        .env("OW_RT_CANARY", mode)
        .output()
        .expect("spawn the test binary")
}

#[test]
fn the_allocation_guard_fires_on_an_allocating_node() {
    let clean = run_canary("clean");
    assert!(
        clean.status.success(),
        "control run failed: {:?}\n{}",
        clean.status,
        String::from_utf8_lossy(&clean.stderr)
    );
    let alloc = run_canary("alloc");
    assert!(
        !alloc.status.success(),
        "an allocating node ran under the guard without aborting"
    );
    // The guard aborts the process; an ordinary test panic would exit with code 101.
    assert_ne!(
        alloc.status.code(),
        Some(101),
        "expected an abort, not a panic"
    );
}

#[test]
fn the_allocation_guard_fires_on_a_worker_thread() {
    let alloc = run_canary("worker-alloc");
    assert!(
        !alloc.status.success(),
        "a node allocating on a DSP worker ran for 20 s without the guard aborting"
    );
    assert_ne!(
        alloc.status.code(),
        Some(101),
        "expected an abort, not a panic: {}",
        String::from_utf8_lossy(&alloc.stderr)
    );
}

/// With workers (`OW_TEST_WORKERS`), a wide graph must actually run on them, so the suite
/// exercised the guard on worker threads; without, nothing runs off the callback thread.
#[test]
fn workers_run_nodes_under_the_guard() {
    let workers = test_workers();
    let mut graph = Graph::new();
    let master = graph.add_node(Box::new(MasterNode::new()));
    for track in 0..32 {
        let osc = graph.add_node(Box::new(OscillatorNode::new(100.0 + track as f32, 0.01)));
        let mut tail = osc;
        for _ in 0..4 {
            let gain = graph.add_node(Box::new(GainNode::new(1, 0.99)));
            graph.connect(tail, 0, gain, 0).unwrap();
            tail = gain;
        }
        graph.connect(tail, 0, master, track % 2).unwrap();
    }
    graph.set_master(master).unwrap();
    let (mut link, mut core) = builder().build();
    assert_eq!(core.workers(), workers);
    link.swap_plan(graph.compile(SAMPLE_RATE, 512).unwrap())
        .unwrap();
    let telemetry = Arc::clone(link.telemetry());
    let mut buffer = vec![0.0_f32; 512 * 2];
    let worker_nodes = || {
        (1..telemetry.threads())
            .map(|thread| telemetry.nodes_run(thread))
            .sum::<u64>()
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut cycles = 0;
    while cycles < 500 || (workers > 0 && worker_nodes() == 0) {
        assert!(Instant::now() < deadline, "no worker ran a node in 60 s");
        core.process(&mut buffer, 2, None);
        cycles += 1;
    }
    if workers == 0 {
        assert_eq!(worker_nodes(), 0);
    }
    assert!(buffer.iter().any(|s| s.abs() > 0.0));
}

#[test]
fn a_full_retire_ring_delays_swaps_without_freeing_on_the_audio_thread() {
    let (mut link, mut core) = builder().plan_queue(4).retire_queue(1).build();
    let telemetry = Arc::clone(link.telemetry());
    for variant in 0..4 {
        link.swap_plan(plan(variant).0).unwrap();
    }
    let mut buffer = vec![0.0_f32; 64 * 2];
    for _ in 0..10 {
        core.process(&mut buffer, 2, None);
    }
    // Plans 0..=2 swapped in; retiring plan 1 found the retire ring full (it holds plan 0), so
    // the fourth swap waits instead of the audio thread dropping a plan.
    assert_eq!(telemetry.plan_swaps(), 3);
    assert!(
        buffer.iter().any(|s| s.abs() > 0.0),
        "audio keeps playing while stalled"
    );

    let mut collected = 0;
    for _ in 0..6 {
        collected += link.collect_retired();
        core.process(&mut buffer, 2, None);
    }
    collected += link.collect_retired();
    assert_eq!(telemetry.plan_swaps(), 4);
    assert_eq!(
        collected, 3,
        "every swapped-out plan came back to the control thread"
    );
}

/// The audio thread writes strip meters, transport position, load and overflow counters into the
/// telemetry bank on every cycle, under the allocation guard (the guard wraps `process`, and the
/// bank writes sit inside the metered faders it runs).
#[test]
fn telemetry_bank_writes_from_metered_faders_are_allocation_free() {
    let mut editor = Editor::new(Project::new("metered"));
    let media = editor.alloc_id().unwrap();
    editor
        .apply(EditCommand::AddMedia {
            item: MediaItem {
                id: media,
                name: "m".into(),
                path: "audio/m.wav".into(),
                hash: "00".into(),
                sample_rate: SAMPLE_RATE,
                channels: 1,
                frames: u64::from(SAMPLE_RATE) * 4,
            },
        })
        .unwrap();
    let track = editor.alloc_id().unwrap();
    editor
        .apply(EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned::new(Track {
                id: track,
                name: "t".into(),
                color: 0,
                kind: TrackKind::Audio,
                strip: Strip::default(),
                instrument: None,
            })),
        })
        .unwrap();
    let clip = editor.alloc_id().unwrap();
    editor
        .apply(EditCommand::AddClip {
            clip: Box::new(Clip {
                id: clip,
                track,
                name: "c".into(),
                start: 0.0,
                length: 8.0,
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

    // The clip plays a four-second sine from a media file; loading happens here, at compile
    // time, never in the guarded cycles below.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("audio")).unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut wav = hound::WavWriter::create(dir.path().join("audio/m.wav"), spec).unwrap();
    for i in 0..SAMPLE_RATE * 4 {
        let phase = std::f64::consts::TAU * 440.0 * f64::from(i) / f64::from(SAMPLE_RATE);
        wav.write_sample((0.5 * phase.sin()) as f32).unwrap();
    }
    wav.finalize().unwrap();
    let ctx = CompileContext::new(dir.path());

    let bank = Arc::new(TelemetryBank::new());
    let compiled =
        compile_project_metered(editor.project(), &ctx, SAMPLE_RATE, MAX_BLOCK, Some(&bank))
            .unwrap();
    assert!(compiled.missing_media.is_empty());
    assert!(compiled.nodes.get(NodeKey::Clips(track)).is_some());
    let (mut link, mut core) = builder().build();
    link.swap_timeline(compiled.timeline).unwrap();
    link.swap_plan(compiled.plan).unwrap();
    assert!(link.send(Message::Transport(TransportCommand::Play)));

    let telemetry = Arc::clone(link.telemetry());
    let mut buffer = vec![0.0_f32; 64 * 2];
    for _ in 0..2000 {
        core.process(&mut buffer, 2, None);
        bank.set_position(telemetry.transport_position());
        bank.set_cpu_load(0.25);
        bank.set_overflow(Overflow::Ring, core.overflow_total());
        bank.set_overflow(Overflow::Unrouted, telemetry.unrouted());
    }
    let slot = meter_slot(editor.project(), MixerRef::Track(track)).unwrap();
    let (peak, rms) = bank.take_meter(slot).unwrap();
    assert!(
        peak[0] > 0.1 && rms[0] > 0.05,
        "meter saw the tone: {peak:?} {rms:?}"
    );
    assert!(bank.take_meter(0).unwrap().0[0] > 0.1, "master meter too");
    assert!(bank.position() > 0, "the transport position was published");
    assert_eq!(bank.cpu_load(), 0.25);
}
