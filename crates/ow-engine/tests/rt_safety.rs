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
                tempo: None,
                key: None,
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
                input: None,
                monitor: Default::default(),
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
                    warp: None,
                },
                take: None,
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

/// The record tap (installing a set, pushing the input of every cycle into the takes, ending and
/// retiring the set) runs under the allocation guard, for an input wider than the take (so the
/// channel-selecting scratch path runs too). The writer thread drains the ring outside it.
#[test]
fn the_record_tap_is_allocation_free() {
    const CYCLES: u64 = 500;
    const FRAMES: u64 = 64;
    const LATENCY: u32 = 100;
    let dir = tempfile::tempdir().unwrap();
    let (writer, producer) = ow_media::RecordWriter::start(
        &dir.path().join("take.wav"),
        2,
        SAMPLE_RATE,
        SAMPLE_RATE as usize,
    )
    .unwrap();
    let (set, shared) = ow_engine::RecordSet::new(
        vec![ow_engine::RecordTrack::new(0, 2, vec![producer])],
        LATENCY,
    );
    let (mut link, mut core) = builder().build();
    link.swap_plan(plan(0).0).unwrap();
    link.swap_record(set).unwrap();
    assert!(link.send(Message::Transport(TransportCommand::SetRecordArm(true))));
    assert!(link.send(Message::Transport(TransportCommand::Play)));

    let mut output = vec![0.0_f32; FRAMES as usize * 2];
    let input = vec![0.25_f32; FRAMES as usize * 3];
    for _ in 0..CYCLES {
        core.process(&mut output, 2, Some(&input));
    }
    assert!(shared.started());
    // Recording began at position 0, so the first `LATENCY` frames belong before the timeline.
    assert_eq!(shared.captured(), CYCLES * FRAMES - u64::from(LATENCY));
    assert_eq!(shared.start_sample(), 0);

    assert!(link.send(Message::EndRecording));
    core.process(&mut output, 2, Some(&input));
    assert!(shared.done());
    // Ending is final: later cycles push nothing.
    core.process(&mut output, 2, Some(&input));
    assert_eq!(shared.captured(), CYCLES * FRAMES - u64::from(LATENCY));
    link.collect_retired();
    let stats = writer.finish().unwrap();
    assert_eq!(stats.frames, shared.captured());
    assert_eq!(stats.dropped_frames, 0);
}

/// The loop browser's preview mixes into the output, ends, is replaced and is stopped under
/// the allocation guard (the preview buffer is handed back, never freed on the audio thread).
#[test]
fn the_preview_is_allocation_free() {
    use ow_engine::Preview;
    let pcm = |frames: usize| ow_media::Pcm {
        sample_rate: SAMPLE_RATE,
        channels: vec![vec![0.5; frames], vec![0.25; frames]],
    };
    let (mut link, mut core) = builder().build();
    link.swap_plan(plan(0).0).unwrap();
    let mut output = vec![0.0_f32; 128];
    let (first, first_handle) = Preview::new(pcm(2_000));
    link.swap_preview(first).unwrap();
    for _ in 0..40 {
        core.process(&mut output, 2, None);
    }
    assert!(first_handle.finished(), "plays out and ends");
    // A preview replaced while playing, then one stopped while playing.
    let (second, second_handle) = Preview::new(pcm(100_000));
    link.swap_preview(second).unwrap();
    core.process(&mut output, 2, None);
    let (third, third_handle) = Preview::new(pcm(100_000));
    link.swap_preview(third).unwrap();
    assert!(
        !second_handle.finished(),
        "the replaced preview fades out first"
    );
    for _ in 0..10 {
        core.process(&mut output, 2, None);
    }
    assert!(second_handle.finished());
    third_handle.stop();
    for _ in 0..40 {
        core.process(&mut output, 2, None);
    }
    assert!(third_handle.finished());
    // Everything handed back is freed on the control thread.
    link.collect_retired();
}

/// Replacing a preview is not a hard cut: the old one fades out under the new one's fade-in,
/// so no sample steps by more than a small amount (a cut from 0.5 to 0 would step by 0.5).
#[test]
fn replacing_a_preview_has_no_discontinuity() {
    use ow_engine::Preview;
    let constant = |frames: usize, value: f32| ow_media::Pcm {
        sample_rate: SAMPLE_RATE,
        channels: vec![vec![value; frames]],
    };
    let (mut link, mut core) = builder().build();
    link.swap_plan(plan(0).0).unwrap();
    let mut heard = Vec::new();
    let mut output = vec![0.0_f32; 128];
    let mut cycle = |core: &mut ow_engine::AudioCore, heard: &mut Vec<f32>| {
        core.process(&mut output, 2, None);
        heard.extend(output.chunks(2).map(|f| f[0]));
    };
    link.swap_preview(Preview::new(constant(100_000, 0.5)).0)
        .unwrap();
    for _ in 0..40 {
        cycle(&mut core, &mut heard);
    }
    link.swap_preview(Preview::new(constant(100_000, 0.5)).0)
        .unwrap();
    for _ in 0..40 {
        cycle(&mut core, &mut heard);
    }
    let biggest = heard
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0_f32, f32::max);
    assert!(biggest < 0.02, "largest step {biggest}");
    link.collect_retired();
}

/// Loop-take switching and input monitoring run under the allocation guard: a short loop wraps
/// every few cycles while recording (each wrap moves the tap to the next take's producer, the
/// ones past the prepared takes are dropped and counted), a monitoring track plays two of the
/// input's channels through its strip, and the count-in is audible through the metronome.
#[test]
fn loop_take_switching_and_input_monitoring_are_allocation_free() {
    use ow_engine::{RecordSet, RecordTrack};
    use ow_model::{InputChannels, MonitorMode, TrackInput};

    const FRAMES: usize = 64;
    const PASSES: usize = 3;
    let mut editor = Editor::new(Project::new("monitor"));
    let track = editor.alloc_id().unwrap();
    editor
        .apply(EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned::new(Track {
                id: track,
                name: "vox".into(),
                color: 0,
                kind: TrackKind::Audio,
                strip: Strip::default(),
                instrument: None,
                input: Some(TrackInput {
                    channels: InputChannels::Stereo(1),
                }),
                monitor: MonitorMode::On,
            })),
        })
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Four input channels; the loop is a quarter of a beat (6000 frames at 120 BPM), so the
    // tap wraps every 93 cycles or so.
    let ctx = CompileContext::new(dir.path()).for_live(4);
    let compiled =
        compile_project_metered(editor.project(), &ctx, SAMPLE_RATE, MAX_BLOCK, None).unwrap();
    assert!(compiled.nodes.get(NodeKey::Monitor(track)).is_some());
    assert!(compiled.nodes.get(NodeKey::Metronome).is_some());

    let mut writers = Vec::new();
    let mut producers = Vec::new();
    for pass in 0..PASSES {
        let (writer, producer) = ow_media::RecordWriter::start(
            &dir.path().join(format!("take-{pass}.wav")),
            2,
            SAMPLE_RATE,
            SAMPLE_RATE as usize,
        )
        .unwrap();
        writers.push(writer);
        producers.push(producer);
    }
    let (set, shared) = RecordSet::new(vec![RecordTrack::new(1, 2, producers)], 0);
    let (mut link, mut core) = builder().build();
    link.swap_timeline(compiled.timeline).unwrap();
    link.swap_plan(compiled.plan).unwrap();
    link.swap_record(set).unwrap();
    for command in [
        TransportCommand::SetLoop {
            enabled: true,
            start_beats: 0.0,
            end_beats: 0.25,
        },
        TransportCommand::SetMetronome {
            enabled: false,
            count_in_bars: 1,
        },
        TransportCommand::SetRecordArm(true),
        TransportCommand::Play,
    ] {
        assert!(link.send(Message::Transport(command)));
    }

    let mut output = vec![0.0_f32; FRAMES * 2];
    let input = vec![0.25_f32; FRAMES * 4];
    let mut peak = 0.0_f32;
    // A bar of count-in at 120 BPM is 96000 frames, then five loop passes of 6000 frames.
    let cycles = (96_000 + 5 * 6_000) / FRAMES + 8;
    for cycle in 0..cycles {
        core.process(&mut output, 2, Some(&input));
        if cycle > 4 {
            peak = peak.max(output.iter().fold(0.0, |m, s| m.max(s.abs())));
        }
        if cycle == 10 {
            assert!(!shared.started(), "nothing is recorded during the count-in");
        }
    }
    assert!(
        peak > 0.2,
        "the monitored input and the clicks are audible: {peak}"
    );
    assert!(shared.started());
    assert_eq!(shared.passes(), PASSES);
    // Six passes began (the last one partly); three had no take to go to.
    assert_eq!(shared.dropped_passes(), 3);
    for pass in 0..PASSES {
        assert_eq!(shared.pass_captured(pass), 6_000, "pass {pass}");
    }
    assert!(link.send(Message::EndRecording));
    core.process(&mut output, 2, Some(&input));
    link.collect_retired();
    for (pass, writer) in writers.into_iter().enumerate() {
        let stats = writer.finish().unwrap();
        assert_eq!(stats.frames, shared.pass_captured(pass));
        assert_eq!(stats.dropped_frames, 0);
    }
}

/// The MIDI-clip player, the synth and the three built-in effects as inserts run under the
/// allocation guard: note events from a dense clip, a loop wrap every pass, locates and stop/play
/// (each of which releases sounding notes) and more simultaneous notes than the synth has voices
/// (so voice stealing runs too).
#[test]
fn the_midi_clip_player_and_synth_are_allocation_free() {
    let mut editor = Editor::new(Project::new("midi"));
    let track = editor.alloc_id().unwrap();
    let synth = editor.alloc_id().unwrap();
    let effects: Vec<ow_model::DeviceId> = (0..3).map(|_| editor.alloc_id().unwrap()).collect();
    let clip = editor.alloc_id().unwrap();
    let notes: Vec<ow_model::Note> = (0..400)
        .map(|i| ow_model::Note {
            start: f64::from(i) * 0.01,
            length: 0.5,
            key: 36 + (i % 48) as u8,
            velocity: 100,
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
                    strip: Strip {
                        inserts: effects.clone(),
                        ..Strip::default()
                    },
                    instrument: Some(synth),
                    input: None,
                    monitor: Default::default(),
                },
                clips: vec![Clip {
                    id: clip,
                    track,
                    name: "notes".into(),
                    start: 0.0,
                    length: 8.0,
                    muted: false,
                    content: ClipContent::Midi {
                        notes,
                        controllers: vec![],
                    },
                    take: None,
                }],
                devices: std::iter::once((synth, "builtin.synth"))
                    .chain(effects.iter().copied().zip([
                        "builtin.eq",
                        "builtin.compressor",
                        "builtin.reverb",
                    ]))
                    .map(|(id, kind)| ow_model::Device {
                        id,
                        kind: kind.into(),
                        name: kind.into(),
                        enabled: true,
                        // Effects with audible settings: a boosted band, a low threshold, a tail.
                        params: match kind {
                            "builtin.eq" => [(1, 6.0), (4, 6.0), (7, -6.0)].into_iter().collect(),
                            "builtin.compressor" => [(0, -30.0)].into_iter().collect(),
                            "builtin.reverb" => [(2, 50.0)].into_iter().collect(),
                            _ => Default::default(),
                        },
                        state: vec![],
                        sidechain: None,
                    })
                    .collect(),
                lanes: vec![],
            }),
        })
        .unwrap();
    let compiled = compile_project_metered(
        editor.project(),
        &CompileContext::default(),
        SAMPLE_RATE,
        MAX_BLOCK,
        None,
    )
    .unwrap();
    assert!(compiled.nodes.get(NodeKey::Device(synth)).is_some());
    for effect in &effects {
        assert!(compiled.nodes.get(NodeKey::Device(*effect)).is_some());
    }
    let (mut link, mut core) = builder().build();
    link.swap_timeline(compiled.timeline).unwrap();
    link.swap_plan(compiled.plan).unwrap();
    assert!(link.send(Message::Transport(TransportCommand::SetLoop {
        enabled: true,
        start_beats: 0.0,
        end_beats: 3.0,
    })));
    assert!(link.send(Message::Transport(TransportCommand::Play)));

    let mut buffer = vec![0.0_f32; 64 * 2];
    let mut heard = 0.0_f32;
    for cycle in 0..3000_u32 {
        match cycle {
            700 => {
                assert!(link.send(Message::Transport(TransportCommand::Locate(5_000))));
            }
            1400 => {
                assert!(link.send(Message::Transport(TransportCommand::Stop)));
            }
            1500 => {
                assert!(link.send(Message::Transport(TransportCommand::Play)));
            }
            _ => {}
        }
        core.process(&mut buffer, 2, None);
        heard = buffer.iter().fold(heard, |m, s| m.max(s.abs()));
        assert!(buffer.iter().all(|s| s.is_finite()));
    }
    assert!(heard > 0.05, "the synth sounded: {heard}");
}

/// The mixing stages under the allocation guard: pan stages, a bus with an input stage and a
/// pre-fader send, a limiter on a track and on the master (so delay compensation inserts delay
/// lines), and a loud signal that keeps the limiter working.
#[test]
fn the_mix_stages_and_the_limiter_are_allocation_free() {
    use ow_model::{AuxSend, Bus, BusId, Device, DeviceBundle, DeviceId, DeviceSlot, Output};
    let mut editor = Editor::new(Project::new("mix"));
    let midi_track = |editor: &mut Editor, pan: f64, output: Output| {
        let track = editor.alloc_id().unwrap();
        let synth = editor.alloc_id().unwrap();
        let clip = editor.alloc_id().unwrap();
        let notes: Vec<ow_model::Note> = (0..64)
            .map(|i| ow_model::Note {
                start: f64::from(i) * 0.05,
                length: 0.5,
                key: 40 + (i % 24) as u8,
                velocity: 127,
            })
            .collect();
        editor
            .apply(EditCommand::InsertTrack {
                index: editor.project().tracks.len(),
                bundle: Box::new(Owned {
                    item: Track {
                        id: track,
                        name: "Keys".into(),
                        color: 0,
                        kind: TrackKind::Midi,
                        strip: Strip {
                            settings: ow_model::StripSettings {
                                pan,
                                output,
                                ..Default::default()
                            },
                            ..Strip::default()
                        },
                        instrument: Some(synth),
                        input: None,
                        monitor: Default::default(),
                    },
                    clips: vec![Clip {
                        id: clip,
                        track,
                        name: "notes".into(),
                        start: 0.0,
                        length: 8.0,
                        muted: false,
                        content: ClipContent::Midi {
                            notes,
                            controllers: vec![],
                        },
                        take: None,
                    }],
                    devices: vec![Device {
                        id: synth,
                        kind: "builtin.synth".into(),
                        name: "synth".into(),
                        enabled: true,
                        params: [(7, 12.0)].into_iter().collect(),
                        state: vec![],
                        sidechain: None,
                    }],
                    lanes: vec![],
                }),
            })
            .unwrap();
        track
    };
    let bus: BusId = editor.alloc_id().unwrap();
    editor
        .apply(EditCommand::InsertBus {
            index: 0,
            bundle: Box::new(Owned {
                item: Bus {
                    id: bus,
                    name: "Bus".into(),
                    color: 0,
                    strip: Strip {
                        settings: ow_model::StripSettings {
                            pan: -0.3,
                            ..Default::default()
                        },
                        sends: vec![],
                        inserts: vec![],
                    },
                },
                clips: vec![],
                devices: vec![],
                lanes: vec![],
            }),
        })
        .unwrap();
    let aux: BusId = editor.alloc_id().unwrap();
    editor
        .apply(EditCommand::InsertBus {
            index: 1,
            bundle: Box::new(Owned {
                item: Bus {
                    id: aux,
                    name: "Aux".into(),
                    color: 0,
                    strip: Strip::default(),
                },
                clips: vec![],
                devices: vec![],
                lanes: vec![],
            }),
        })
        .unwrap();
    let first = midi_track(&mut editor, 0.6, Output::Bus(bus));
    let second = midi_track(&mut editor, -0.8, Output::Master);
    editor
        .apply(EditCommand::SetSends {
            target: MixerRef::Bus(bus),
            sends: vec![AuxSend {
                target: aux,
                level_db: -6.0,
                pre_fader: true,
            }],
        })
        .unwrap();
    editor
        .apply(EditCommand::SetSends {
            target: MixerRef::Track(second),
            sends: vec![AuxSend {
                target: bus,
                level_db: -3.0,
                pre_fader: false,
            }],
        })
        .unwrap();
    let mut limiters: Vec<DeviceId> = Vec::new();
    for strip in [MixerRef::Track(first), MixerRef::Master] {
        let id: DeviceId = editor.alloc_id().unwrap();
        limiters.push(id);
        editor
            .apply(EditCommand::InsertDevice {
                slot: DeviceSlot::Insert { strip, index: 0 },
                bundle: Box::new(DeviceBundle {
                    device: Device {
                        id,
                        kind: "builtin.limiter".into(),
                        name: "limiter".into(),
                        enabled: true,
                        params: [(0, -6.0), (2, 0.004)].into_iter().collect(),
                        state: vec![],
                        sidechain: None,
                    },
                    lanes: vec![],
                }),
            })
            .unwrap();
    }
    let compiled = compile_project_metered(
        editor.project(),
        &CompileContext::default(),
        SAMPLE_RATE,
        MAX_BLOCK,
        None,
    )
    .unwrap();
    assert!(
        compiled
            .nodes
            .get(NodeKey::Pan(MixerRef::Track(first)))
            .is_some()
    );
    assert!(
        compiled
            .nodes
            .get(NodeKey::Pan(MixerRef::Bus(bus)))
            .is_some()
    );
    assert!(
        compiled
            .nodes
            .get(NodeKey::Input(MixerRef::Bus(bus)))
            .is_some()
    );
    assert!(compiled.plan.output_latency() >= 2 * 192);
    let (mut link, mut core) = builder().build();
    link.swap_timeline(compiled.timeline).unwrap();
    link.swap_plan(compiled.plan).unwrap();
    assert!(link.send(Message::Transport(TransportCommand::SetLoop {
        enabled: true,
        start_beats: 0.0,
        end_beats: 2.0,
    })));
    assert!(link.send(Message::Transport(TransportCommand::Play)));
    let mut buffer = vec![0.0_f32; 64 * 2];
    let mut heard = 0.0_f32;
    let ceiling = 10.0_f32.powf(-6.0 / 20.0);
    for cycle in 0..3000_u32 {
        if cycle == 1400 {
            assert!(link.send(Message::Transport(TransportCommand::Stop)));
        }
        if cycle == 1500 {
            assert!(link.send(Message::Transport(TransportCommand::Play)));
        }
        core.process(&mut buffer, 2, None);
        heard = buffer.iter().fold(heard, |m, s| m.max(s.abs()));
        assert!(buffer.iter().all(|s| s.is_finite()));
        assert!(buffer.iter().all(|s| s.abs() <= ceiling), "cycle {cycle}");
    }
    assert!(heard > 0.05, "the mix sounded: {heard}");
}

/// Live MIDI input under the allocation guard: messages stamped on the stream's frame clock reach
/// an armed MIDI track's synth (a flip of the arming flag in the middle releases held keys) and
/// the MIDI half of the record tap across loop wraps, with nothing allocated or freed.
#[test]
fn live_midi_input_and_the_midi_tap_are_allocation_free() {
    let mut editor = Editor::new(Project::new("live midi"));
    let track = editor.alloc_id().unwrap();
    let synth = editor.alloc_id().unwrap();
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
                clips: vec![],
                devices: vec![ow_model::Device {
                    id: synth,
                    kind: "builtin.synth".into(),
                    name: "Synth".into(),
                    enabled: true,
                    params: Default::default(),
                    state: vec![],
                    sidechain: None,
                }],
                lanes: vec![],
            }),
        })
        .unwrap();
    let ctx = CompileContext::default();
    let armed = ctx.armed.clone();
    armed.set(track, true);
    let compiled =
        compile_project_metered(editor.project(), &ctx, SAMPLE_RATE, MAX_BLOCK, None).unwrap();

    let mut core_builder = builder().midi_clock(ow_engine::MidiClock::StreamFrames);
    let mut midi = core_builder.producer_ring(1024);
    let (mut link, mut core) = core_builder.build();
    link.swap_timeline(compiled.timeline).unwrap();
    link.swap_plan(compiled.plan).unwrap();
    let (sender, mut recorded) = ow_engine::producer_ring(1 << 14);
    let (set, shared) = ow_engine::RecordSet::new(vec![], 0);
    link.swap_record(set.with_midi(sender)).unwrap();
    assert!(link.send(Message::Transport(TransportCommand::SetLoop {
        enabled: true,
        start_beats: 0.0,
        end_beats: 1.0,
    })));
    assert!(link.send(Message::Transport(TransportCommand::SetRecordArm(true))));
    assert!(link.send(Message::Transport(TransportCommand::Play)));

    let stamp = |frame: u64| (frame as f64 * 1e6 / f64::from(SAMPLE_RATE)).round() as u64;
    let mut buffer = vec![0.0_f32; 64 * 2];
    let mut heard = 0.0_f32;
    for cycle in 0..2000_u64 {
        if cycle % 500 == 250 {
            // Disarm while keys are held, then arm again.
            armed.set(track, false);
        } else if cycle % 500 == 0 {
            armed.set(track, true);
        }
        let key = 36 + (cycle % 48) as u8;
        for (frame, status, data2) in [(cycle * 64 + 10, 0x90, 100), (cycle * 64 + 70, 0x80, 0)] {
            midi.push(Message::MidiIn {
                message: MidiMessage {
                    status,
                    data1: key,
                    data2,
                },
                timestamp_us: stamp(frame),
            });
        }
        core.process(&mut buffer, 2, None);
        heard = buffer.iter().fold(heard, |m, s| m.max(s.abs()));
        assert!(buffer.iter().all(|s| s.is_finite()));
    }
    assert!(heard > 0.05, "the armed synth played the input: {heard}");
    assert!(shared.midi_passes() > 3, "the loop wrapped while recording");
    assert!(link.send(Message::EndRecording));
    core.process(&mut buffer, 2, None);
    assert!(shared.done());
    link.collect_retired();
    let mut events = 0;
    while recorded.pop().is_some() {
        events += 1;
    }
    assert!(events > 1000, "{events} messages recorded");
}

/// Both built-in synthesizers, with every modulation path on (unison, LFO, filter envelope,
/// feedback), playing live MIDI through the engine cycle under the allocation guard.
#[test]
fn the_built_in_synthesizers_are_allocation_free_in_the_cycle() {
    let synth_params: std::collections::BTreeMap<u32, f64> = [
        (8, 1.0),
        (13, 0.7),
        (17, 0.5),
        (20, 0.1),
        (23, 36.0),
        (28, 4.0),
        (30, 0.3),
        (31, 12.0),
        (32, 0.3),
        (33, 6.0),
        (35, 0.8),
    ]
    .into();
    let fm_params: std::collections::BTreeMap<u32, f64> =
        [(0, 4.0), (1, 0.5), (13, 0.6), (21, 0.5), (29, 0.7)].into();
    for (kind, params) in [("builtin.synth", synth_params), ("builtin.fm", fm_params)] {
        let mut editor = Editor::new(Project::new("synths"));
        let track = editor.alloc_id().unwrap();
        let device = editor.alloc_id().unwrap();
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
                        instrument: Some(device),
                        input: None,
                        monitor: Default::default(),
                    },
                    clips: vec![],
                    devices: vec![ow_model::Device {
                        id: device,
                        kind: kind.into(),
                        name: "Synth".into(),
                        enabled: true,
                        params,
                        state: vec![],
                        sidechain: None,
                    }],
                    lanes: vec![],
                }),
            })
            .unwrap();
        let ctx = CompileContext::default();
        ctx.armed.set(track, true);
        let compiled =
            compile_project_metered(editor.project(), &ctx, SAMPLE_RATE, MAX_BLOCK, None).unwrap();
        let mut core_builder = builder().midi_clock(ow_engine::MidiClock::StreamFrames);
        let mut midi = core_builder.producer_ring(1024);
        let (mut link, mut core) = core_builder.build();
        link.swap_timeline(compiled.timeline).unwrap();
        link.swap_plan(compiled.plan).unwrap();
        assert!(link.send(Message::Transport(TransportCommand::Play)));
        let stamp = |frame: u64| (frame as f64 * 1e6 / f64::from(SAMPLE_RATE)).round() as u64;
        let mut buffer = vec![0.0_f32; 64 * 2];
        let mut heard = 0.0_f32;
        for cycle in 0..1500_u64 {
            let key = 36 + (cycle % 48) as u8;
            for (frame, status, data2) in [(cycle * 64 + 10, 0x90, 100), (cycle * 64 + 70, 0x80, 0)]
            {
                midi.push(Message::MidiIn {
                    message: MidiMessage {
                        status,
                        data1: key,
                        data2,
                    },
                    timestamp_us: stamp(frame),
                });
            }
            core.process(&mut buffer, 2, None);
            heard = buffer.iter().fold(heard, |m, s| m.max(s.abs()));
            assert!(buffer.iter().all(|s| s.is_finite()), "{kind} cycle {cycle}");
        }
        assert!(heard > 0.02, "{kind} played the input: {heard}");
    }
}
