//! Plugin delay compensation, computed when a graph is compiled.
//!
//! Every node reports a latency in samples ([`Node::latency_samples`]). Walking the graph in
//! topological order, the arrival latency of an input edge is the output latency of its source;
//! a node's input latency is the largest arrival over its input edges, and its output latency is
//! that plus its own latency. Every input edge that arrives earlier than the node's input latency
//! gets a [`DelayNode`] of the difference, so all inputs of every node (including the
//! compiler-inserted sums, i.e. every mixer input) line up to the sample. The master node's
//! output latency is the plan's [`output_latency`](crate::RenderPlan::output_latency).
//!
//! Delay lines are allocated here, on the control thread; latency changes need a recompile.

use crate::graph::{Node, ProcessContext};

/// Delays to insert: `(edge index, samples)`, plus each node's output latency.
pub(crate) struct Compensation {
    pub(crate) delays: Vec<(usize, u32)>,
    pub(crate) output_latency: Vec<u32>,
}

/// Computes the compensating delays for a graph with per-node `latency`, `edges` as
/// `(from, to)` node pairs (one entry per channel edge), and a topological `order`.
pub(crate) fn compensate(
    latency: &[u32],
    edges: &[(usize, usize)],
    order: &[usize],
) -> Compensation {
    let count = latency.len();
    let mut incoming: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (index, &(_, to)) in edges.iter().enumerate() {
        incoming[to].push(index);
    }
    let mut output_latency = vec![0_u32; count];
    let mut delays = Vec::new();
    for &node in order {
        let input_latency = incoming[node]
            .iter()
            .map(|&edge| output_latency[edges[edge].0])
            .max()
            .unwrap_or(0);
        for &edge in &incoming[node] {
            let arrival = output_latency[edges[edge].0];
            if arrival < input_latency {
                delays.push((edge, input_latency - arrival));
            }
        }
        output_latency[node] = input_latency.saturating_add(latency[node]);
    }
    Compensation {
        delays,
        output_latency,
    }
}

/// Mono delay line the compiler inserts on a shorter path. Allocated in `new`.
#[derive(Debug, Clone)]
pub struct DelayNode {
    line: Box<[f32]>,
    position: usize,
}

impl DelayNode {
    /// A delay of `samples` (at least 1) samples, starting silent.
    pub fn new(samples: u32) -> Self {
        Self {
            line: vec![0.0; samples.max(1) as usize].into_boxed_slice(),
            position: 0,
        }
    }

    /// The delay in samples.
    pub fn delay(&self) -> u32 {
        self.line.len() as u32
    }
}

impl Node for DelayNode {
    fn name(&self) -> &'static str {
        "delay-compensation"
    }
    fn input_channels(&self) -> usize {
        1
    }
    fn output_channels(&self) -> usize {
        1
    }
    /// The delay itself. The compiler inserts these nodes after computing compensation, so this
    /// value is informational and never compensated again.
    fn latency_samples(&self) -> u32 {
        self.delay()
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let input = ctx.inputs.channel(0);
        let output = ctx.outputs.channel_mut(0);
        let len = self.line.len();
        let mut position = self.position;
        for (out, &sample) in output.iter_mut().zip(input) {
            *out = self.line[position];
            self.line[position] = sample;
            position += 1;
            if position == len {
                position = 0;
            }
        }
        self.position = position;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorter_paths_into_a_node_are_delayed_to_the_longest() {
        // 0 -> 1 (latency 512) -> 3 and 0 -> 2 (latency 0) -> 3 and 0 -> 3 directly.
        let latency = [0, 512, 0, 0];
        let edges = [(0, 1), (0, 2), (1, 3), (2, 3), (0, 3)];
        let result = compensate(&latency, &edges, &[0, 1, 2, 3]);
        assert_eq!(result.delays, vec![(3, 512), (4, 512)]);
        assert_eq!(result.output_latency, vec![0, 512, 0, 512]);
    }

    #[test]
    fn latencies_accumulate_along_a_chain_without_delays() {
        let latency = [10, 20, 30];
        let result = compensate(&latency, &[(0, 1), (1, 2)], &[0, 1, 2]);
        assert!(result.delays.is_empty());
        assert_eq!(result.output_latency, vec![10, 30, 60]);
    }
}
