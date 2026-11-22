//! The compiled, immutable-shape render plan.

use std::mem;

use crate::graph::{Inputs, Node, NodeId, Outputs, ProcessContext, Transport};

/// Buffer id that always reads as silence; never written.
pub(crate) const SILENCE: u32 = 0;

pub(crate) struct PlannedNode {
    pub(crate) node: Box<dyn Node>,
    pub(crate) in_start: usize,
    pub(crate) in_len: usize,
    pub(crate) out_start: usize,
    pub(crate) out_len: usize,
}

/// Everything the audio thread needs for a graph, built by [`Graph::compile`](crate::Graph::compile)
/// on the control thread and then sent to the audio thread over a wait-free ring.
///
/// A plan holds the nodes in dependency order, the buffer each node channel reads and writes
/// (buffers are reused once their last reader has run), and the master output channels. Its
/// shape never changes after compilation; the audio thread only runs it. Replacing the graph
/// means compiling a new plan and swapping it in at a cycle boundary; the old plan travels back
/// to the control thread through the retire ring so it is dropped (and freed) off the audio
/// thread.
///
/// Node state (oscillator phase, filter memory) lives in the plan's nodes, so a recompiled plan
/// starts its nodes from their constructor state unless the control thread carries state over.
pub struct RenderPlan {
    pub(crate) sample_rate: u32,
    pub(crate) max_block: usize,
    pub(crate) nodes: Vec<PlannedNode>,
    /// Flat list of buffer ids; each node owns an input range and an output range.
    pub(crate) io: Vec<u32>,
    pub(crate) pool: Vec<Box<[f32]>>,
    /// Holds a node's output buffers while it runs; capacity reserved at compile time.
    pub(crate) scratch: Vec<Box<[f32]>>,
    /// `(id, index into nodes)` sorted by id, for allocation-free message routing.
    pub(crate) lookup: Vec<(NodeId, usize)>,
    pub(crate) master: Vec<u32>,
}

impl RenderPlan {
    /// Sample rate the plan was compiled for.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Largest block the plan's buffers hold.
    pub fn max_block(&self) -> usize {
        self.max_block
    }

    /// Number of nodes, including compiler-inserted sums.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of audio buffers (channels) the plan owns, including the silence buffer.
    pub fn buffer_count(&self) -> usize {
        self.pool.len()
    }

    /// Number of master output channels.
    pub fn master_channels(&self) -> usize {
        self.master.len()
    }

    /// Latency in samples reported by a node, if it is in the plan.
    pub fn latency_of(&self, id: NodeId) -> Option<u32> {
        self.index_of(id)
            .map(|index| self.nodes[index].node.latency_samples())
    }

    /// Samples of master channel `channel` from the most recent block.
    pub(crate) fn master_channel(&self, channel: usize, frames: usize) -> Option<&[f32]> {
        self.master
            .get(channel)
            .map(|&id| &self.pool[id as usize][..frames])
    }

    fn index_of(&self, id: NodeId) -> Option<usize> {
        self.lookup
            .binary_search_by_key(&id, |entry| entry.0)
            .ok()
            .map(|position| self.lookup[position].1)
    }

    /// Routes a parameter change to a node; false when the node is not in this plan.
    pub(crate) fn set_param(&mut self, id: NodeId, param: u32, value: f32) -> bool {
        match self.index_of(id) {
            Some(index) => {
                self.nodes[index].node.set_param(param, value);
                true
            }
            None => false,
        }
    }

    /// Routes a MIDI message to a node; false when the node is not in this plan.
    pub(crate) fn midi(&mut self, id: NodeId, message: crate::graph::MidiMessage) -> bool {
        match self.index_of(id) {
            Some(index) => {
                self.nodes[index].node.on_midi(message);
                true
            }
            None => false,
        }
    }

    /// Runs every node once for `frames` (`1..=max_block`). Allocation-free.
    pub(crate) fn process_block(&mut self, frames: usize, transport: Transport) {
        debug_assert!(frames >= 1 && frames <= self.max_block);
        let Self {
            nodes,
            io,
            pool,
            scratch,
            sample_rate,
            ..
        } = self;
        for planned in nodes.iter_mut() {
            let out_ids = &io[planned.out_start..planned.out_start + planned.out_len];
            let in_ids = &io[planned.in_start..planned.in_start + planned.in_len];
            scratch.clear();
            for &id in out_ids {
                scratch.push(mem::take(&mut pool[id as usize]));
            }
            {
                let mut ctx = ProcessContext {
                    frames,
                    sample_rate: *sample_rate,
                    transport,
                    inputs: Inputs {
                        pool: &pool[..],
                        ids: in_ids,
                        frames,
                    },
                    outputs: Outputs {
                        bufs: &mut scratch[..],
                        frames,
                    },
                };
                planned.node.process(&mut ctx);
            }
            for (buffer, &id) in scratch.drain(..).zip(out_ids) {
                pool[id as usize] = buffer;
            }
        }
    }
}
