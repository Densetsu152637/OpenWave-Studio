//! Control-thread graph description and its compilation into a [`RenderPlan`].

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::fmt;

use super::nodes::SumNode;
use super::{Node, NodeId};
use crate::pdc::{self, DelayNode};
use crate::plan::{Buffers, NodeCell, PlanData, PlannedNode, RenderPlan, SILENCE};
use crate::sched::dag::Dag;

/// Why a graph could not be edited or compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphError {
    /// An edge or the master refers to a node that was never added.
    UnknownNode(NodeId),
    /// An edge names a channel the node does not have.
    ChannelOutOfRange {
        /// The node.
        node: NodeId,
        /// The offending channel.
        channel: usize,
    },
    /// The edges form a cycle.
    Cycle,
    /// No master node was set.
    NoMaster,
    /// `max_block` or `sample_rate` was zero.
    InvalidFormat,
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNode(id) => write!(f, "unknown node {}", id.0),
            Self::ChannelOutOfRange { node, channel } => {
                write!(f, "node {} has no channel {channel}", node.0)
            }
            Self::Cycle => f.write_str("the graph contains a cycle"),
            Self::NoMaster => f.write_str("no master node was set"),
            Self::InvalidFormat => f.write_str("sample rate and block size must be non-zero"),
        }
    }
}

impl std::error::Error for GraphError {}

#[derive(Clone, Copy)]
struct Edge {
    from: usize,
    from_ch: usize,
    to: usize,
    to_ch: usize,
}

struct Slot {
    id: NodeId,
    node: Box<dyn Node>,
}

/// Editable graph owned by the control thread. [`compile`](Self::compile) consumes it.
///
/// Several edges into one input channel are summed by a compiler-inserted
/// [`SumNode`] (inputs in edge order, so the sum is the same on every run); an input with no edge
/// reads silence. Paths of different latency into one node are aligned by compiler-inserted
/// [`DelayNode`]s. The master node's output channels become the plan's output.
#[derive(Default)]
pub struct Graph {
    slots: Vec<Slot>,
    edges: Vec<Edge>,
    master: Option<usize>,
    next_id: u32,
}

impl Graph {
    /// An empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node and returns its id (ids count up from 0 in insertion order).
    pub fn add_node(&mut self, node: Box<dyn Node>) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        self.slots.push(Slot { id, node });
        id
    }

    fn position(&self, id: NodeId) -> Result<usize, GraphError> {
        self.slots
            .iter()
            .position(|slot| slot.id == id)
            .ok_or(GraphError::UnknownNode(id))
    }

    /// Connects output channel `from_ch` of `from` to input channel `to_ch` of `to`.
    pub fn connect(
        &mut self,
        from: NodeId,
        from_ch: usize,
        to: NodeId,
        to_ch: usize,
    ) -> Result<(), GraphError> {
        let from_pos = self.position(from)?;
        let to_pos = self.position(to)?;
        if from_ch >= self.slots[from_pos].node.output_channels() {
            return Err(GraphError::ChannelOutOfRange {
                node: from,
                channel: from_ch,
            });
        }
        if to_ch >= self.slots[to_pos].node.input_channels() {
            return Err(GraphError::ChannelOutOfRange {
                node: to,
                channel: to_ch,
            });
        }
        self.edges.push(Edge {
            from: from_pos,
            from_ch,
            to: to_pos,
            to_ch,
        });
        Ok(())
    }

    /// Chooses the node whose output channels are the plan's output.
    pub fn set_master(&mut self, node: NodeId) -> Result<(), GraphError> {
        self.master = Some(self.position(node)?);
        Ok(())
    }

    /// Orders the nodes, inserts delay compensation, assigns and reuses buffers, builds the
    /// dependency graph for the parallel scheduler, calls [`Node::prepare`] on every node and
    /// returns the plan. Allocates freely: run it on the control thread.
    ///
    /// Buffer reuse is liveness-aware: a node's output takes over a released buffer only when
    /// every earlier user of that buffer (its writer and all its readers) is an ancestor of the
    /// node in the graph. Reuse therefore never adds a hidden write-after-read or
    /// write-after-write dependency between parallel branches, and the scheduler only has to
    /// honour the graph's own edges.
    pub fn compile(mut self, sample_rate: u32, max_block: usize) -> Result<RenderPlan, GraphError> {
        if sample_rate == 0 || max_block == 0 {
            return Err(GraphError::InvalidFormat);
        }
        let master_slot = self.master.ok_or(GraphError::NoMaster)?;
        self.insert_sums();
        let order = self.topological_order()?;
        let output_latency = self.insert_delays(&order)[master_slot];
        let order = self.topological_order()?;

        let count = self.slots.len();
        // Position of every slot in the plan (topological order).
        let mut position_of = vec![0_usize; count];
        for (position, &slot) in order.iter().enumerate() {
            position_of[slot] = position;
        }
        let master = position_of[master_slot];
        let inputs_of: Vec<usize> = order
            .iter()
            .map(|&slot| self.slots[slot].node.input_channels())
            .collect();
        let outputs_of: Vec<usize> = order
            .iter()
            .map(|&slot| self.slots[slot].node.output_channels())
            .collect();
        // After `insert_sums` every input channel has at most one edge.
        let mut source: Vec<Vec<Option<(usize, usize)>>> =
            inputs_of.iter().map(|&inputs| vec![None; inputs]).collect();
        let mut readers: Vec<Vec<usize>> =
            outputs_of.iter().map(|&outputs| vec![0; outputs]).collect();
        let mut dependencies = Vec::with_capacity(self.edges.len());
        for edge in &self.edges {
            let (from, to) = (position_of[edge.from], position_of[edge.to]);
            source[to][edge.to_ch] = Some((from, edge.from_ch));
            readers[from][edge.from_ch] += 1;
            dependencies.push((from, to));
        }
        let ancestors = Ancestors::new(count, &dependencies);

        let mut buffer_count = 1_u32; // buffer 0 is silence
        // Released buffers and the nodes that used them in their last lifetime.
        let mut free: Vec<(u32, Vec<usize>)> = Vec::new();
        let mut users: Vec<Vec<usize>> = vec![Vec::new()];
        let mut out_ids: Vec<Vec<u32>> = vec![Vec::new(); count];
        let mut io: Vec<u32> = Vec::new();
        let mut ranges = Vec::with_capacity(count);

        for node in 0..count {
            let in_start = io.len();
            for channel_source in &source[node] {
                io.push(channel_source.map_or(SILENCE, |(from, ch)| out_ids[from][ch]));
            }
            // Outputs are taken before this node's inputs are released, so they never alias.
            let out_start = io.len();
            for _ in 0..outputs_of[node] {
                let reusable = free
                    .iter()
                    .position(|(_, previous)| previous.iter().all(|&u| ancestors.has(node, u)));
                let id = match reusable {
                    Some(index) => free.remove(index).0,
                    None => {
                        buffer_count += 1;
                        users.push(Vec::new());
                        buffer_count - 1
                    }
                };
                users[id as usize] = vec![node];
                out_ids[node].push(id);
                io.push(id);
            }
            ranges.push((in_start, inputs_of[node], out_start, outputs_of[node]));
            // Release inputs whose last reader this was (the master's outputs stay live).
            for &(from, ch) in source[node].iter().flatten() {
                let id = out_ids[from][ch];
                users[id as usize].push(node);
                readers[from][ch] -= 1;
                if readers[from][ch] == 0 && from != master {
                    free.push((id, std::mem::take(&mut users[id as usize])));
                }
            }
            // Outputs nobody reads (and that are not the master's) are free straight away.
            if node != master {
                for (ch, &id) in out_ids[node].iter().enumerate() {
                    if readers[node][ch] == 0 {
                        free.push((id, std::mem::take(&mut users[id as usize])));
                    }
                }
            }
        }

        let master_ids = out_ids[master].clone().into_boxed_slice();
        let mut lookup: Vec<(NodeId, usize)> = Vec::with_capacity(count);
        let mut latencies = Vec::with_capacity(count);
        let mut slots: Vec<Option<Slot>> = self.slots.into_iter().map(Some).collect();
        let mut nodes = Vec::with_capacity(count);
        for (position, &slot_index) in order.iter().enumerate() {
            let mut slot = slots[slot_index].take().expect("each node is placed once");
            slot.node.prepare(sample_rate, max_block);
            let (in_start, in_len, out_start, out_len) = ranges[position];
            lookup.push((slot.id, position));
            latencies.push(slot.node.latency_samples());
            nodes.push(NodeCell::new(PlannedNode {
                node: slot.node,
                in_start,
                in_len,
                out_start,
                out_len,
            }));
        }
        lookup.sort_by_key(|entry| entry.0);

        Ok(RenderPlan::new(PlanData::new(
            sample_rate,
            max_block,
            nodes.into_boxed_slice(),
            io.into_boxed_slice(),
            Buffers::new(buffer_count as usize, max_block),
            lookup.into_boxed_slice(),
            latencies.into_boxed_slice(),
            master_ids,
            output_latency,
            Dag::new(count, &dependencies),
        )))
    }

    /// Inserts a [`DelayNode`] on every edge that reaches its node earlier than the node's
    /// slowest input (see [`crate::pdc`]) and returns each original slot's output latency.
    fn insert_delays(&mut self, order: &[usize]) -> Vec<u32> {
        let latency: Vec<u32> = self
            .slots
            .iter()
            .map(|slot| slot.node.latency_samples())
            .collect();
        let pairs: Vec<(usize, usize)> = self.edges.iter().map(|e| (e.from, e.to)).collect();
        let compensation = pdc::compensate(&latency, &pairs, order);
        for (edge_index, samples) in compensation.delays {
            let delay_slot = self.slots.len();
            let id = NodeId(self.next_id);
            self.next_id += 1;
            self.slots.push(Slot {
                id,
                node: Box::new(DelayNode::new(samples)),
            });
            let edge = self.edges[edge_index];
            self.edges[edge_index].to = delay_slot;
            self.edges[edge_index].to_ch = 0;
            self.edges.push(Edge {
                from: delay_slot,
                from_ch: 0,
                to: edge.to,
                to_ch: edge.to_ch,
            });
        }
        compensation.output_latency
    }

    /// Replaces fan-in (several edges into one input) with a [`SumNode`].
    fn insert_sums(&mut self) {
        let mut by_input: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
        for (edge_index, edge) in self.edges.iter().enumerate() {
            by_input
                .entry((edge.to, edge.to_ch))
                .or_default()
                .push(edge_index);
        }
        for ((to, to_ch), edge_ids) in by_input {
            if edge_ids.len() < 2 {
                continue;
            }
            let sum_slot = self.slots.len();
            let id = NodeId(self.next_id);
            self.next_id += 1;
            self.slots.push(Slot {
                id,
                node: Box::new(SumNode::new(edge_ids.len())),
            });
            for (input, &edge_index) in edge_ids.iter().enumerate() {
                self.edges[edge_index].to = sum_slot;
                self.edges[edge_index].to_ch = input;
            }
            self.edges.push(Edge {
                from: sum_slot,
                from_ch: 0,
                to,
                to_ch,
            });
        }
    }

    /// Kahn's algorithm, always taking the lowest-index ready node so output is deterministic.
    fn topological_order(&self) -> Result<Vec<usize>, GraphError> {
        let count = self.slots.len();
        let mut pending = vec![0_usize; count];
        let mut successors: Vec<Vec<usize>> = vec![Vec::new(); count];
        for edge in &self.edges {
            pending[edge.to] += 1;
            successors[edge.from].push(edge.to);
        }
        let mut ready: BinaryHeap<Reverse<usize>> = (0..count)
            .filter(|&index| pending[index] == 0)
            .map(Reverse)
            .collect();
        let mut order = Vec::with_capacity(count);
        while let Some(Reverse(next)) = ready.pop() {
            order.push(next);
            for &to in &successors[next] {
                pending[to] -= 1;
                if pending[to] == 0 {
                    ready.push(Reverse(to));
                }
            }
        }
        if order.len() < count {
            return Err(GraphError::Cycle);
        }
        Ok(order)
    }
}

/// Ancestor sets of every node, as bit rows, for the liveness-aware buffer assignment.
struct Ancestors {
    words: usize,
    bits: Vec<u64>,
}

impl Ancestors {
    /// `edges` go from lower to higher node indices (topological numbering).
    fn new(count: usize, edges: &[(usize, usize)]) -> Self {
        let words = count.div_ceil(64).max(1);
        let mut predecessors: Vec<Vec<usize>> = vec![Vec::new(); count];
        for &(from, to) in edges {
            predecessors[to].push(from);
        }
        let mut bits = vec![0_u64; count * words];
        for (node, froms) in predecessors.iter().enumerate() {
            let (done, rest) = bits.split_at_mut(node * words);
            let row = &mut rest[..words];
            for &from in froms {
                debug_assert!(from < node);
                for (word, ancestor) in row.iter_mut().zip(&done[from * words..(from + 1) * words])
                {
                    *word |= ancestor;
                }
                row[from / 64] |= 1_u64 << (from % 64);
            }
        }
        Self { words, bits }
    }

    /// Whether `ancestor` precedes `node` along some path of edges.
    fn has(&self, node: usize, ancestor: usize) -> bool {
        self.bits[node * self.words + ancestor / 64] & (1_u64 << (ancestor % 64)) != 0
    }
}
