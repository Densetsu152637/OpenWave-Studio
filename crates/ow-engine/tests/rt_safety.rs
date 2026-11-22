//! Real-time safety: the engine cycle must not allocate (or free) with plan swaps in flight or
//! with a producer ring overflowing.
//!
//! `AllocDisabler` is installed as this test binary's global allocator, and `AudioCore::process`
//! runs every cycle under `assert_no_alloc`, which aborts the process on any allocation,
//! reallocation or deallocation inside the cycle.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use assert_no_alloc::AllocDisabler;
use ow_engine::graph::nodes::{GAIN_PARAM, GainNode, MasterNode, OscillatorNode};
use ow_engine::{
    CoreBuilder, Graph, MAX_BLOCK, Message, MidiMessage, NodeId, RenderPlan, TransportCommand,
};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const SAMPLE_RATE: u32 = 48_000;

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
    let builder = CoreBuilder::new().plan_queue(4).control_ring(256);
    let (mut link, mut core) = builder.build();
    let telemetry = Arc::clone(link.telemetry());
    let stop = Arc::new(AtomicBool::new(false));
    let stop_audio = Arc::clone(&stop);

    // The "audio thread": a plain thread calling the cycle in a tight loop with small buffers.
    let audio = thread::spawn(move || {
        let mut buffer = vec![0.0_f32; 64 * 2];
        let mut peak = 0.0_f32;
        while !stop_audio.load(Ordering::Relaxed) {
            core.process(&mut buffer, 2);
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
    let mut builder = CoreBuilder::new().control_ring(8);
    let mut midi = builder.producer_ring(4);
    let (mut link, mut core) = builder.build();
    assert_eq!(core.ring_count(), 2);
    let (first, gain) = plan(1);
    link.swap_plan(first).unwrap();
    let mut buffer = vec![0.0_f32; 128 * 2];
    core.process(&mut buffer, 2);

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
        });
    }
    assert_eq!(link.message_overflow(), 92);
    assert_eq!(midi.overflow_count(), 96);

    for _ in 0..50 {
        core.process(&mut buffer, 2);
    }
    assert_eq!(core.overflow_total(), 92 + 96);

    // Still healthy and still accepting messages after the overflow.
    assert!(link.send(Message::Transport(TransportCommand::Play)));
    core.process(&mut buffer, 2);
    assert!(core.transport().playing);
}

#[test]
fn oversized_buffers_and_empty_cores_are_allocation_free() {
    let (mut link, mut core) = CoreBuilder::new().build();
    // No plan yet: silence, no allocation.
    let mut big = vec![1.0_f32; (MAX_BLOCK * 2 + 17) * 2];
    core.process(&mut big, 2);
    assert!(big.iter().all(|s| *s == 0.0));
    // A device buffer larger than the plan block is chunked inside the cycle.
    link.swap_plan(plan(0).0).unwrap();
    core.process(&mut big, 2);
    assert!(big.iter().any(|s| s.abs() > 0.01));
    // Mono and wide device layouts.
    let mut mono = vec![0.0_f32; 256];
    core.process(&mut mono, 1);
    let mut wide = vec![0.0_f32; 256 * 6];
    core.process(&mut wide, 6);
}
