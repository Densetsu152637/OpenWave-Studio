//! Parallel scheduling (plan stage 6): renders are bit-identical for any worker count, work
//! really spreads across the workers, and the scheduler keeps its invariants under stress
//! (every node once per cycle, after its predecessors, never on two threads at once, reading
//! exactly what its predecessors wrote), and a node panicking on a worker aborts the process.

use std::cell::Cell;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ow_engine::graph::nodes::{GAIN_PARAM, GainNode, MasterNode, OscillatorNode};
use ow_engine::{
    Graph, Message, Node, NodeId, OfflineRenderer, ProcessContext, RenderPlan, TransportCommand,
};

const SAMPLE_RATE: u32 = 48_000;

/// A stateful, deliberately expensive mono effect: a one-pole low-pass followed by a soft
/// clipper, iterated `passes` times per sample so a node costs enough to be worth sharing.
struct Burn {
    coefficient: f32,
    passes: usize,
    state: f32,
}

impl Burn {
    fn new(coefficient: f32, passes: usize) -> Self {
        Self {
            coefficient,
            passes,
            state: 0.0,
        }
    }
}

impl Node for Burn {
    fn name(&self) -> &'static str {
        "burn"
    }
    fn input_channels(&self) -> usize {
        1
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let input = ctx.inputs.channel(0);
        let output = ctx.outputs.channel_mut(0);
        for (out, &sample) in output.iter_mut().zip(input) {
            let mut value = sample;
            for _ in 0..self.passes {
                self.state += self.coefficient * (value - self.state);
                value = self.state / (1.0 + self.state.abs());
            }
            *out = value;
        }
    }
}

/// A mixer-like graph, wide and deep: `tracks` tracks of oscillator -> burn -> gain -> burn,
/// each feeding one of four group buses and a shared send bus (fan-in sums), the buses feeding
/// both master channels. Returns the graph and the gain node of track 0.
fn session(tracks: usize) -> (Graph, NodeId) {
    let mut graph = Graph::new();
    let master = graph.add_node(Box::new(MasterNode::new()));
    let buses: Vec<NodeId> = (0..4)
        .map(|bus| graph.add_node(Box::new(Burn::new(0.3 + 0.1 * bus as f32, 4))))
        .collect();
    let send = graph.add_node(Box::new(Burn::new(0.05, 8)));
    let mut first_gain = None;
    for track in 0..tracks {
        let osc = graph.add_node(Box::new(OscillatorNode::new(
            55.0 + 37.5 * track as f32,
            0.2,
        )));
        let drive = graph.add_node(Box::new(Burn::new(0.2 + 0.01 * track as f32, 16)));
        let gain = graph.add_node(Box::new(GainNode::new(1, 0.5 + 0.02 * track as f32)));
        let tone = graph.add_node(Box::new(Burn::new(0.6, 8)));
        graph.connect(osc, 0, drive, 0).unwrap();
        graph.connect(drive, 0, gain, 0).unwrap();
        graph.connect(gain, 0, tone, 0).unwrap();
        graph.connect(tone, 0, buses[track % 4], 0).unwrap();
        graph.connect(gain, 0, send, 0).unwrap();
        first_gain.get_or_insert(gain);
    }
    for (index, &bus) in buses.iter().enumerate() {
        graph.connect(bus, 0, master, index % 2).unwrap();
    }
    graph.connect(send, 0, master, 0).unwrap();
    graph.connect(send, 0, master, 1).unwrap();
    graph.set_master(master).unwrap();
    (graph, first_gain.unwrap())
}

struct Render {
    samples: Vec<f32>,
    /// Nodes run by worker threads (not the calling thread).
    worker_nodes: u64,
}

fn render(workers: usize, block: usize) -> Render {
    let (graph, gain) = session(24);
    let plan = graph.compile(SAMPLE_RATE, 512).unwrap();
    let mut renderer = OfflineRenderer::with_workers(plan, 2, workers);
    let mut samples = renderer.render(24_000, block);
    // A parameter change between blocks, so message routing runs alongside the workers.
    renderer.send(Message::SetParam {
        node: gain,
        param: GAIN_PARAM,
        value: 0.1,
    });
    samples.extend(renderer.render(24_000, block));
    let telemetry = renderer.link().telemetry();
    assert_eq!(telemetry.threads(), workers + 1);
    let worker_nodes = (1..telemetry.threads())
        .map(|thread| telemetry.nodes_run(thread))
        .sum();
    Render {
        samples,
        worker_nodes,
    }
}

fn assert_bit_identical(reference: &[f32], other: &[f32], label: &str) {
    assert_eq!(reference.len(), other.len(), "{label}: length");
    if let Some(index) = reference
        .iter()
        .zip(other)
        .position(|(a, b)| a.to_bits() != b.to_bits())
    {
        panic!(
            "{label}: sample {index} differs: {} vs {}",
            reference[index], other[index]
        );
    }
}

#[test]
fn renders_are_bit_identical_for_1_2_and_8_workers() {
    let serial = render(0, 128);
    assert_eq!(serial.worker_nodes, 0);
    assert!(
        serial.samples.iter().any(|s| s.abs() > 0.01),
        "the session is audible"
    );
    for workers in [1, 2, 8] {
        let parallel = render(workers, 128);
        assert_bit_identical(
            &serial.samples,
            &parallel.samples,
            &format!("{workers} workers"),
        );
        assert!(
            parallel.worker_nodes > 0,
            "{workers} workers: no node ran on a worker, so nothing ran in parallel"
        );
    }
}

#[test]
fn parallel_renders_are_also_independent_of_the_device_block_size() {
    // The gain ramp after the parameter change spans one block, so only the first half (before
    // the change) is comparable across block sizes.
    let serial = render(0, 128);
    let parallel = render(8, 333);
    assert_bit_identical(
        &serial.samples[..24_000 * 2],
        &parallel.samples[..24_000 * 2],
        "8 workers, block 333",
    );
}

/// The value node `index` writes in the cycle with `stamp` (exact in `f32`).
fn signature(index: usize, stamp: u64) -> f32 {
    (index * 8 + (stamp % 8) as usize) as f32
}

/// Checks the scheduler's guarantees from inside the nodes. Never panics (a panicking node
/// aborts the process); violations are counted instead.
struct Checker {
    index: usize,
    predecessors: Vec<usize>,
    shared: Arc<CheckState>,
}

struct CheckState {
    /// Cycle stamp of each node's most recent run.
    last_run: Vec<AtomicU64>,
    runs: Vec<AtomicU64>,
    busy: Vec<AtomicBool>,
    violations: AtomicU64,
}

impl Node for Checker {
    fn name(&self) -> &'static str {
        "checker"
    }
    fn input_channels(&self) -> usize {
        self.predecessors.len().max(1)
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let state = &self.shared;
        // Unique per cycle: the transport is playing and advances every cycle.
        let stamp = ctx.transport.position + 1;
        if state.busy[self.index].swap(true, Ordering::AcqRel) {
            state.violations.fetch_add(1, Ordering::Relaxed);
        }
        for &predecessor in &self.predecessors {
            if state.last_run[predecessor].load(Ordering::Acquire) != stamp {
                state.violations.fetch_add(1, Ordering::Relaxed);
            }
        }
        if state.last_run[self.index].load(Ordering::Acquire) == stamp {
            // Ran twice in one cycle.
            state.violations.fetch_add(1, Ordering::Relaxed);
        }
        // Every input must hold exactly what its predecessor wrote this cycle: a buffer shared
        // with a concurrently running node (aliasing) would show up here.
        for (channel, &predecessor) in self.predecessors.iter().enumerate() {
            let expected = signature(predecessor, stamp);
            if ctx.inputs.channel(channel).iter().any(|&s| s != expected) {
                state.violations.fetch_add(1, Ordering::Relaxed);
            }
        }
        ctx.outputs
            .channel_mut(0)
            .fill(signature(self.index, stamp));
        state.runs[self.index].fetch_add(1, Ordering::Relaxed);
        state.last_run[self.index].store(stamp, Ordering::Release);
        state.busy[self.index].store(false, Ordering::Release);
    }
}

/// A random DAG of `count` checker nodes (deterministic generator), all sinks summed into the
/// master. Returns the plan and its check state.
fn random_dag(count: usize, seed: u64) -> (RenderPlan, Arc<CheckState>) {
    let mut rng = seed;
    let mut next = move |bound: usize| {
        rng = rng
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((rng >> 33) as usize) % bound.max(1)
    };
    let predecessors: Vec<Vec<usize>> = (0..count)
        .map(|index| {
            let mut list: Vec<usize> = (0..next(4)).map(|_| next(index)).collect();
            list.retain(|_| index > 0);
            list.sort_unstable();
            list.dedup();
            list
        })
        .collect();
    let state = Arc::new(CheckState {
        last_run: (0..count).map(|_| AtomicU64::new(0)).collect(),
        runs: (0..count).map(|_| AtomicU64::new(0)).collect(),
        busy: (0..count).map(|_| AtomicBool::new(false)).collect(),
        violations: AtomicU64::new(0),
    });
    let mut graph = Graph::new();
    let ids: Vec<NodeId> = predecessors
        .iter()
        .enumerate()
        .map(|(index, list)| {
            graph.add_node(Box::new(Checker {
                index,
                predecessors: list.clone(),
                shared: Arc::clone(&state),
            }))
        })
        .collect();
    let mut has_successor = vec![false; count];
    for (index, list) in predecessors.iter().enumerate() {
        for (input, &from) in list.iter().enumerate() {
            graph.connect(ids[from], 0, ids[index], input).unwrap();
            has_successor[from] = true;
        }
    }
    let master = graph.add_node(Box::new(MasterNode::new()));
    for (index, &id) in ids.iter().enumerate() {
        if !has_successor[index] {
            graph.connect(id, 0, master, index % 2).unwrap();
        }
    }
    graph.set_master(master).unwrap();
    (graph.compile(SAMPLE_RATE, 64).unwrap(), state)
}

#[test]
fn scheduler_invariants_hold_under_stress_and_plan_swaps() {
    const PLANS: u64 = 12;
    const FRAMES: usize = 32 * 150;
    let size = |plan: u64| {
        if plan == 0 {
            300
        } else {
            200 + 25 * plan as usize
        }
    };
    // 8 workers, swapping to a new random DAG after each segment.
    let (first, first_state) = random_dag(size(0), 1);
    let mut renderer = OfflineRenderer::with_workers(first, 2, 8);
    renderer.send(Message::Transport(TransportCommand::Play));
    let mut segments = Vec::new();
    let mut state = first_state;
    for plan in 0..=PLANS {
        let start = renderer.transport().position;
        let out = renderer.render(FRAMES, 32);
        segments.push((plan, start, out, Arc::clone(&state)));
        if plan < PLANS {
            let (next, next_state) = random_dag(size(plan + 1), plan + 2);
            assert!(renderer.link().swap_plan(next).is_ok());
            renderer.link().collect_retired();
            state = next_state;
        }
    }
    renderer.link().collect_retired();
    let telemetry = renderer.link().telemetry();
    let worker_nodes: u64 = (1..telemetry.threads())
        .map(|t| telemetry.nodes_run(t))
        .sum();
    assert!(worker_nodes > 0, "the workers took part");

    for (plan, start, parallel, state) in segments {
        assert_eq!(
            state.violations.load(Ordering::Relaxed),
            0,
            "plan {plan}: scheduler invariant violated (8 workers)"
        );
        let runs: Vec<u64> = state
            .runs
            .iter()
            .map(|r| r.load(Ordering::Relaxed))
            .collect();
        assert!(
            runs.iter().all(|&r| r == runs[0] && r > 0),
            "plan {plan}: nodes ran unequal numbers of times"
        );
        // The same DAG rendered serially from the same position must match bit for bit.
        let (reference_plan, reference_state) = random_dag(size(plan), plan + 1);
        let mut reference = OfflineRenderer::new(reference_plan, 2);
        reference.send(Message::Transport(TransportCommand::Play));
        reference.send(Message::Transport(TransportCommand::Locate(start)));
        let serial = reference.render(FRAMES, 32);
        assert_eq!(reference_state.violations.load(Ordering::Relaxed), 0);
        assert_bit_identical(&serial, &parallel, &format!("plan {plan}: 0 vs 8 workers"));
    }
}

thread_local! {
    /// Set on the thread that drives the panic canary's cycles.
    static ON_CALLBACK_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Panics, but only when run off the callback thread (on a DSP worker), if `panic` is set.
struct PanicOnWorker {
    panic: bool,
}

impl Node for PanicOnWorker {
    fn name(&self) -> &'static str {
        "panic-on-worker"
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
        if self.panic && !ON_CALLBACK_THREAD.get() {
            panic!("node panicked on a DSP worker");
        }
    }
}

/// Child half of the panic canary; does nothing unless `OW_PANIC_CANARY` is set. `panic`: nodes
/// panic on workers (the process must abort); `clean`: same graph without panics, for 2 s
/// (exit 0), proving the harness works.
#[test]
fn panic_canary_child() {
    let Ok(mode) = std::env::var("OW_PANIC_CANARY") else {
        return;
    };
    let mut graph = Graph::new();
    let master = graph.add_node(Box::new(MasterNode::new()));
    for index in 0..64 {
        let node = graph.add_node(Box::new(PanicOnWorker {
            panic: mode == "panic",
        }));
        graph.connect(node, 0, master, index % 2).unwrap();
    }
    graph.set_master(master).unwrap();
    let mut renderer =
        OfflineRenderer::with_workers(graph.compile(SAMPLE_RATE, 256).unwrap(), 2, 2);
    ON_CALLBACK_THREAD.set(true);
    let limit = if mode == "panic" { 20 } else { 2 };
    let deadline = Instant::now() + Duration::from_secs(limit);
    while Instant::now() < deadline {
        renderer.render(256, 256);
    }
}

/// Runs the canary child in `mode`; `None` when it had to be killed after 60 s.
fn run_panic_canary(mode: &str) -> Option<ExitStatus> {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "panic_canary_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("OW_PANIC_CANARY", mode)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the test binary");
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("wait for the child") {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

#[test]
fn a_node_panicking_on_a_worker_aborts_the_process_instead_of_hanging() {
    let clean = run_panic_canary("clean").expect("the clean child finished");
    assert!(clean.success(), "control run failed: {clean:?}");
    let panicked = run_panic_canary("panic").expect("the cycle hung instead of aborting");
    assert!(!panicked.success(), "the worker never panicked within 20 s");
    assert_ne!(
        panicked.code(),
        Some(101),
        "expected an abort, not a test panic"
    );
}
