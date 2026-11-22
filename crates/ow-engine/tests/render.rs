//! Offline render tests (plan stage 5 acceptance: oscillator -> gain -> master within -90 dBFS).
//!
//! The reference is analytic, not a committed render: `tests/fixtures/graphs/osc_gain_master.txt`
//! holds the graph parameters, and the expected samples are `amp * gain * sin(2 pi f n / sr)`
//! evaluated in `f64`.

use std::collections::HashMap;
use std::f64::consts::TAU;

use ow_engine::graph::nodes::OSC_PARAM_FREQ;
use ow_engine::graph::nodes::{GAIN_PARAM, GainNode, MasterNode, OscillatorNode, SumNode};
use ow_engine::{
    Graph, GraphError, Message, Node, NodeId, OfflineRenderer, ProcessContext, TransportCommand,
};

/// -90 dBFS as a linear amplitude.
const TOLERANCE: f64 = 3.162_277_660_168_379_4e-5;

struct Fixture(HashMap<String, f64>);

impl Fixture {
    fn load() -> Self {
        let text = include_str!("fixtures/graphs/osc_gain_master.txt");
        let map = text
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .map(|line| {
                let (key, value) = line.split_once('=').expect("key = value");
                (key.trim().to_owned(), value.trim().parse().expect("number"))
            })
            .collect();
        Self(map)
    }
    fn get(&self, key: &str) -> f64 {
        self.0[key]
    }
}

fn osc_gain_master(fixture: &Fixture) -> (Graph, NodeId) {
    let mut graph = Graph::new();
    let osc = graph.add_node(Box::new(OscillatorNode::new(
        fixture.get("osc_freq") as f32,
        fixture.get("osc_amp") as f32,
    )));
    let gain = graph.add_node(Box::new(GainNode::new(1, fixture.get("gain") as f32)));
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(osc, 0, gain, 0).unwrap();
    graph.connect(gain, 0, master, 0).unwrap();
    graph.connect(gain, 0, master, 1).unwrap();
    graph.set_master(master).unwrap();
    (graph, gain)
}

fn expected(fixture: &Fixture, n: usize) -> f64 {
    let phase = fixture.get("osc_freq") * n as f64 / fixture.get("sample_rate");
    fixture.get("osc_amp") * fixture.get("gain") * (TAU * phase).sin()
}

fn max_error(fixture: &Fixture, interleaved: &[f32]) -> f64 {
    interleaved
        .chunks(2)
        .enumerate()
        .flat_map(|(n, frame)| {
            let want = expected(fixture, n);
            frame.iter().map(move |s| (f64::from(*s) - want).abs())
        })
        .fold(0.0, f64::max)
}

fn render(fixture: &Fixture, block: usize) -> Vec<f32> {
    let (graph, _) = osc_gain_master(fixture);
    let plan = graph
        .compile(fixture.get("sample_rate") as u32, 4096)
        .unwrap();
    OfflineRenderer::new(plan, 2).render(fixture.get("frames") as usize, block)
}

#[test]
fn oscillator_gain_master_matches_the_analytic_reference() {
    let fixture = Fixture::load();
    let out = render(&fixture, 512);
    let error = max_error(&fixture, &out);
    assert!(
        error < TOLERANCE,
        "max error {error:e} ({:.1} dBFS) exceeds -90 dBFS",
        20.0 * error.max(1e-12).log10()
    );
    assert!(out.iter().any(|s| s.abs() > 0.3), "render is not silent");
}

#[test]
fn render_is_bit_identical_across_device_block_sizes() {
    let fixture = Fixture::load();
    let reference = render(&fixture, 512);
    for block in [1, 64, 333, 4096, 10_000] {
        assert_eq!(render(&fixture, block), reference, "block size {block}");
    }
}

#[test]
fn parameter_message_changes_gain_with_a_ramp() {
    let fixture = Fixture::load();
    let (graph, gain) = osc_gain_master(&fixture);
    let plan = graph.compile(48_000, 4096).unwrap();
    let mut renderer = OfflineRenderer::new(plan, 2);
    let before = renderer.render(4800, 4800);
    assert!(renderer.send(Message::SetParam {
        node: gain,
        param: GAIN_PARAM,
        value: 0.0,
    }));
    let after = renderer.render(4800, 4800);
    assert!(before.iter().any(|s| s.abs() > 0.1));
    // Ramps to silence across the block, ends at exactly zero.
    assert_eq!(after[after.len() - 1], 0.0);
    let jump = after
        .chunks(2)
        .map(|f| f[0])
        .collect::<Vec<_>>()
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0_f32, f32::max);
    // A 1 kHz sine at 0.4 peak already steps up to ~0.052 per sample; a gain jump would add more.
    assert!(jump < 0.06, "ramp is smooth, max step {jump}");
    let peak = |s: &[f32]| s.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
    assert!(peak(&after[after.len() - 96..]) < 0.1 * peak(&after[..96]));
}

#[test]
fn fan_in_is_summed_and_unconnected_inputs_are_silent() {
    let mut graph = Graph::new();
    let a = graph.add_node(Box::new(OscillatorNode::new(100.0, 0.25)));
    let b = graph.add_node(Box::new(OscillatorNode::new(100.0, 0.25)));
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(a, 0, master, 0).unwrap();
    graph.connect(b, 0, master, 0).unwrap();
    graph.set_master(master).unwrap();
    let plan = graph.compile(48_000, 256).unwrap();
    let out = OfflineRenderer::new(plan, 2).render(1000, 100);
    for (n, frame) in out.chunks(2).enumerate() {
        let want = 0.5 * (TAU * 100.0 * n as f64 / 48_000.0).sin();
        assert!((f64::from(frame[0]) - want).abs() < 1e-6, "frame {n}");
        assert_eq!(frame[1], 0.0, "unconnected right input is silent");
    }
}

#[test]
fn buffers_are_reused_along_a_chain() {
    let mut graph = Graph::new();
    let osc = graph.add_node(Box::new(OscillatorNode::new(440.0, 0.5)));
    let mut previous = (osc, 0);
    for _ in 0..16 {
        let gain = graph.add_node(Box::new(GainNode::new(1, 1.0)));
        graph.connect(previous.0, previous.1, gain, 0).unwrap();
        previous = (gain, 0);
    }
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(previous.0, 0, master, 0).unwrap();
    graph.set_master(master).unwrap();
    let plan = graph.compile(48_000, 256).unwrap();
    // silence + osc/gain ping-pong + master pair, not one buffer per node.
    assert!(plan.buffer_count() <= 6, "buffers: {}", plan.buffer_count());
}

#[test]
fn compile_rejects_bad_graphs() {
    let mut graph = Graph::new();
    let a = graph.add_node(Box::new(GainNode::new(1, 1.0)));
    let b = graph.add_node(Box::new(GainNode::new(1, 1.0)));
    graph.connect(a, 0, b, 0).unwrap();
    graph.connect(b, 0, a, 0).unwrap();
    assert_eq!(
        graph.connect(a, 3, b, 0),
        Err(GraphError::ChannelOutOfRange {
            node: a,
            channel: 3
        })
    );
    assert_eq!(
        graph.connect(NodeId(99), 0, b, 0),
        Err(GraphError::UnknownNode(NodeId(99)))
    );
    graph.set_master(b).unwrap();
    assert_eq!(graph.compile(48_000, 64).err(), Some(GraphError::Cycle));

    let mut no_master = Graph::new();
    no_master.add_node(Box::new(SumNode::new(2)));
    assert_eq!(
        no_master.compile(48_000, 64).err(),
        Some(GraphError::NoMaster)
    );
}

#[test]
fn transport_messages_move_the_position() {
    let fixture = Fixture::load();
    let (graph, _) = osc_gain_master(&fixture);
    let plan = graph.compile(48_000, 4096).unwrap();
    let mut renderer = OfflineRenderer::new(plan, 2);
    renderer.render(100, 100);
    assert!(renderer.send(Message::Transport(TransportCommand::Locate(1000))));
    assert!(renderer.send(Message::Transport(TransportCommand::Play)));
    renderer.render(500, 128);
    assert_eq!(renderer.transport().position, 1500);
    assert!(renderer.send(Message::Transport(TransportCommand::Stop)));
    renderer.render(100, 100);
    assert!(!renderer.transport().playing);
    assert_eq!(renderer.transport().position, 1500);
    assert_eq!(renderer.link().message_overflow(), 0);
}

fn set(node: NodeId, param: u32, value: f32) -> Message {
    Message::SetParam { node, param, value }
}

#[test]
fn non_finite_gain_is_ignored_and_output_recovers() {
    let fixture = Fixture::load();
    let (graph, gain) = osc_gain_master(&fixture);
    let plan = graph.compile(48_000, 4096).unwrap();
    let mut renderer = OfflineRenderer::new(plan, 2);
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(renderer.send(set(gain, GAIN_PARAM, bad)));
    }
    let during = renderer.render(2400, 256);
    assert!(during.iter().all(|s| s.is_finite()));
    assert!(
        during.iter().any(|s| s.abs() > 0.1),
        "gain was not poisoned"
    );
    assert!(renderer.send(set(gain, GAIN_PARAM, 0.25)));
    let after = renderer.render(4800, 256);
    assert!(after.iter().all(|s| s.is_finite()));
    let peak = after[after.len() - 960..]
        .iter()
        .fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!((peak - 0.8 * 0.25).abs() < 0.01, "peak {peak}");
}

#[test]
fn oscillator_rejects_bad_frequency_and_amplitude() {
    let mut graph = Graph::new();
    let osc = graph.add_node(Box::new(OscillatorNode::new(1000.0, 0.5)));
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(osc, 0, master, 0).unwrap();
    graph.set_master(master).unwrap();
    let plan = graph.compile(48_000, 256).unwrap();
    let mut renderer = OfflineRenderer::new(plan, 2);
    assert!(renderer.send(set(osc, OSC_PARAM_FREQ, f32::NAN)));
    assert!(renderer.send(set(osc, OSC_PARAM_FREQ, -250.0)));
    assert!(renderer.send(set(osc, 1, f32::INFINITY)));
    let out = renderer.render(4800, 100);
    assert!(out.iter().all(|s| s.is_finite()));
    let crossings = out
        .chunks(2)
        .map(|f| f[0])
        .collect::<Vec<_>>()
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    assert!(
        (99..=101).contains(&crossings),
        "still 1 kHz: {crossings} crossings in 0.1 s"
    );
    let peak = out.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!((peak - 0.5).abs() < 0.01);
}

/// Emits NaN and infinity, to prove the device-buffer interleave contains them.
struct FaultyNode;

impl Node for FaultyNode {
    fn name(&self) -> &'static str {
        "faulty"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        for (i, sample) in ctx.outputs.channel_mut(0).iter_mut().enumerate() {
            *sample = match i % 3 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                _ => 0.5,
            };
        }
    }
}

#[test]
fn non_finite_samples_never_reach_the_device_buffer() {
    let mut graph = Graph::new();
    let faulty = graph.add_node(Box::new(FaultyNode));
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(faulty, 0, master, 0).unwrap();
    graph.connect(faulty, 0, master, 1).unwrap();
    graph.set_master(master).unwrap();
    let plan = graph.compile(48_000, 256).unwrap();
    let out = OfflineRenderer::new(plan, 2).render(600, 200);
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(out.contains(&0.5), "finite samples still pass through");
}

#[test]
fn device_input_is_accepted_and_does_not_change_output() {
    let fixture = Fixture::load();
    let reference = render(&fixture, 128);
    let (graph, _) = osc_gain_master(&fixture);
    let plan = graph.compile(48_000, 4096).unwrap();
    let frames = fixture.get("frames") as usize;
    let input = vec![0.7_f32; frames * 2];
    let out = OfflineRenderer::new(plan, 2).render_with_input(frames, 128, Some(&input));
    assert_eq!(out, reference);
}
