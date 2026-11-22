//! Plugin delay compensation (plan stage 6): a 512-sample-latency branch lines up with a
//! zero-latency branch to the sample, whether the branches meet at a sum or at two channels of
//! one node, and whatever the worker count or block size.

use ow_engine::graph::nodes::{GainNode, MasterNode};
use ow_engine::{Graph, Node, OfflineRenderer, ProcessContext, RenderPlan};

const LATENCY: u32 = 512;

/// One 1.0 sample at the very start, then silence.
struct Impulse {
    fired: bool,
}

impl Node for Impulse {
    fn name(&self) -> &'static str {
        "impulse"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let out = ctx.outputs.channel_mut(0);
        out.fill(0.0);
        if !self.fired {
            out[0] = 1.0;
            self.fired = true;
        }
    }
}

/// A "plugin" with a lookahead: delays its input by `latency` samples and reports it.
struct Lookahead {
    line: Vec<f32>,
    position: usize,
}

impl Lookahead {
    fn new(latency: u32) -> Self {
        Self {
            line: vec![0.0; latency as usize],
            position: 0,
        }
    }
}

impl Node for Lookahead {
    fn name(&self) -> &'static str {
        "lookahead"
    }
    fn input_channels(&self) -> usize {
        1
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn latency_samples(&self) -> u32 {
        self.line.len() as u32
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let input = ctx.inputs.channel(0);
        let output = ctx.outputs.channel_mut(0);
        for (out, &sample) in output.iter_mut().zip(input) {
            *out = self.line[self.position];
            self.line[self.position] = sample;
            self.position = (self.position + 1) % self.line.len();
        }
    }
}

fn impulse(graph: &mut Graph) -> ow_engine::NodeId {
    graph.add_node(Box::new(Impulse { fired: false }))
}

/// Indices of the non-zero samples of channel `channel` in an interleaved stereo render.
fn hits(out: &[f32], channel: usize) -> Vec<(usize, f32)> {
    out.chunks(2)
        .enumerate()
        .filter(|(_, frame)| frame[channel] != 0.0)
        .map(|(index, frame)| (index, frame[channel]))
        .collect()
}

/// Latent branch to the left master channel, plain branch to the right.
fn split_channels() -> RenderPlan {
    let mut graph = Graph::new();
    let source = impulse(&mut graph);
    let latent = graph.add_node(Box::new(Lookahead::new(LATENCY)));
    let plain = graph.add_node(Box::new(GainNode::new(1, 1.0)));
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(source, 0, latent, 0).unwrap();
    graph.connect(source, 0, plain, 0).unwrap();
    graph.connect(latent, 0, master, 0).unwrap();
    graph.connect(plain, 0, master, 1).unwrap();
    graph.set_master(master).unwrap();
    graph.compile(48_000, 256).unwrap()
}

/// Latent and plain branches summed into the left master channel.
fn summed() -> RenderPlan {
    let mut graph = Graph::new();
    let source = impulse(&mut graph);
    let latent = graph.add_node(Box::new(Lookahead::new(LATENCY)));
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(source, 0, latent, 0).unwrap();
    graph.connect(latent, 0, master, 0).unwrap();
    graph.connect(source, 0, master, 0).unwrap();
    graph.set_master(master).unwrap();
    graph.compile(48_000, 256).unwrap()
}

#[test]
fn a_512_sample_branch_aligns_with_a_zero_latency_branch_on_two_channels() {
    for (workers, block) in [(0, 128), (0, 100), (4, 128), (4, 1000)] {
        let plan = split_channels();
        assert_eq!(plan.output_latency(), LATENCY);
        let out = OfflineRenderer::with_workers(plan, 2, workers).render(2048, block);
        let label = format!("{workers} workers, block {block}");
        assert_eq!(hits(&out, 0), vec![(512, 1.0)], "{label}: latent branch");
        assert_eq!(
            hits(&out, 1),
            vec![(512, 1.0)],
            "{label}: compensated branch"
        );
    }
}

#[test]
fn a_512_sample_branch_aligns_with_a_zero_latency_branch_at_a_sum() {
    for workers in [0, 4] {
        let plan = summed();
        assert_eq!(plan.output_latency(), LATENCY);
        let out = OfflineRenderer::with_workers(plan, 2, workers).render(2048, 128);
        // Both copies of the impulse land on the same sample: one hit of 2.0, not two of 1.0.
        assert_eq!(hits(&out, 0), vec![(512, 2.0)], "{workers} workers");
    }
}

#[test]
fn chained_latencies_add_up_and_only_the_shorter_path_is_delayed() {
    let mut graph = Graph::new();
    let source = impulse(&mut graph);
    let long = graph.add_node(Box::new(Lookahead::new(LATENCY)));
    let short_a = graph.add_node(Box::new(Lookahead::new(128)));
    let short_b = graph.add_node(Box::new(Lookahead::new(128)));
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(source, 0, long, 0).unwrap();
    graph.connect(source, 0, short_a, 0).unwrap();
    graph.connect(short_a, 0, short_b, 0).unwrap();
    graph.connect(long, 0, master, 0).unwrap();
    graph.connect(short_b, 0, master, 1).unwrap();
    graph.set_master(master).unwrap();
    let plan = graph.compile(48_000, 256).unwrap();
    // Five user nodes plus one 256-sample compensation delay on the short path.
    assert_eq!(plan.node_count(), 6);
    assert_eq!(plan.output_latency(), LATENCY);
    let out = OfflineRenderer::new(plan, 2).render(2048, 64);
    assert_eq!(hits(&out, 0), vec![(512, 1.0)]);
    assert_eq!(hits(&out, 1), vec![(512, 1.0)]);
}

#[test]
fn graphs_without_latency_get_no_delay_nodes() {
    let mut graph = Graph::new();
    let source = impulse(&mut graph);
    let gain = graph.add_node(Box::new(GainNode::new(1, 1.0)));
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(source, 0, gain, 0).unwrap();
    graph.connect(gain, 0, master, 0).unwrap();
    graph.connect(source, 0, master, 1).unwrap();
    graph.set_master(master).unwrap();
    let plan = graph.compile(48_000, 256).unwrap();
    assert_eq!(plan.node_count(), 3);
    assert_eq!(plan.output_latency(), 0);
}
