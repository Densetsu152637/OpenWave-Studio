//! Control-thread graph description and its compilation into a [`RenderPlan`].

use std::collections::BTreeMap;
use std::fmt;

use super::nodes::SumNode;
use super::{Node, NodeId};
use crate::plan::{PlannedNode, RenderPlan, SILENCE};

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
/// [`SumNode`]; an input with no edge reads silence. The master node's output channels become
/// the plan's output.
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

    /// Orders the nodes, assigns and reuses buffers, calls [`Node::prepare`] on every node and
    /// returns the plan. Allocates freely: run it on the control thread.
    pub fn compile(mut self, sample_rate: u32, max_block: usize) -> Result<RenderPlan, GraphError> {
        if sample_rate == 0 || max_block == 0 {
            return Err(GraphError::InvalidFormat);
        }
        let master = self.master.ok_or(GraphError::NoMaster)?;
        self.insert_sums();

        let count = self.slots.len();
        let order = self.topological_order(count)?;

        // Consumers per source channel, to know when a buffer can be reused.
        let mut readers: BTreeMap<(usize, usize), usize> = BTreeMap::new();
        for edge in &self.edges {
            *readers.entry((edge.from, edge.from_ch)).or_insert(0) += 1;
        }

        let mut pool_len = 1_u32; // buffer 0 is silence
        let mut free: Vec<u32> = Vec::new();
        let mut out_ids: Vec<Vec<u32>> = vec![Vec::new(); count];
        let mut io: Vec<u32> = Vec::new();
        let mut ranges: Vec<(usize, usize, usize, usize)> = vec![(0, 0, 0, 0); count];
        let mut max_outputs = 0;

        for &index in &order {
            let node = &self.slots[index].node;
            // Inputs: the source's buffer, or silence.
            let in_start = io.len();
            let mut sources = Vec::with_capacity(node.input_channels());
            for channel in 0..node.input_channels() {
                let source = self
                    .edges
                    .iter()
                    .find(|e| e.to == index && e.to_ch == channel)
                    .map(|e| (e.from, e.from_ch));
                sources.push(source);
                io.push(source.map_or(SILENCE, |(from, ch)| out_ids[from][ch]));
            }
            // Outputs: take from the free list before this node's inputs are released.
            let out_start = io.len();
            let outputs = node.output_channels();
            max_outputs = max_outputs.max(outputs);
            for _ in 0..outputs {
                let id = free.pop().unwrap_or_else(|| {
                    pool_len += 1;
                    pool_len - 1
                });
                out_ids[index].push(id);
                io.push(id);
            }
            ranges[index] = (in_start, node.input_channels(), out_start, outputs);
            // Release inputs whose last reader this was.
            for source in sources.into_iter().flatten() {
                let remaining = readers.get_mut(&source).expect("edge was counted");
                *remaining -= 1;
                if *remaining == 0 && source.0 != master {
                    free.push(out_ids[source.0][source.1]);
                }
            }
            // Outputs nobody reads (and that are not the master's) are free straight away.
            if index != master {
                for (channel, &id) in out_ids[index].iter().enumerate() {
                    if !readers.contains_key(&(index, channel)) {
                        free.push(id);
                    }
                }
            }
        }

        let master_ids = out_ids[master].clone();
        let mut lookup: Vec<(NodeId, usize)> = Vec::with_capacity(count);
        let mut slots: Vec<Option<Slot>> = self.slots.into_iter().map(Some).collect();
        let mut nodes = Vec::with_capacity(count);
        for (position, &index) in order.iter().enumerate() {
            let mut slot = slots[index].take().expect("each node is placed once");
            slot.node.prepare(sample_rate, max_block);
            let (in_start, in_len, out_start, out_len) = ranges[index];
            lookup.push((slot.id, position));
            nodes.push(PlannedNode {
                node: slot.node,
                in_start,
                in_len,
                out_start,
                out_len,
            });
        }
        lookup.sort_by_key(|entry| entry.0);

        let pool = (0..pool_len)
            .map(|_| vec![0.0_f32; max_block].into_boxed_slice())
            .collect();
        Ok(RenderPlan {
            sample_rate,
            max_block,
            nodes,
            io,
            pool,
            scratch: Vec::with_capacity(max_outputs),
            lookup,
            master: master_ids,
        })
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
    fn topological_order(&self, count: usize) -> Result<Vec<usize>, GraphError> {
        let mut pending = vec![0_usize; count];
        for edge in &self.edges {
            pending[edge.to] += 1;
        }
        let mut done = vec![false; count];
        let mut order = Vec::with_capacity(count);
        while order.len() < count {
            let next = (0..count)
                .find(|&i| !done[i] && pending[i] == 0)
                .ok_or(GraphError::Cycle)?;
            done[next] = true;
            order.push(next);
            for edge in self.edges.iter().filter(|e| e.from == next) {
                pending[edge.to] -= 1;
            }
        }
        Ok(order)
    }
}
