//! The compiled, immutable-shape render plan and how one cycle of it runs.

// Nodes and audio buffers are shared between the callback thread and the DSP workers behind
// `UnsafeCell`; the scheduler (`sched::dag`) and the buffer assignment in `Graph::compile`
// guarantee exclusive access. Each `unsafe` block states the invariant it relies on.
#![allow(unsafe_code)]

use std::cell::UnsafeCell;
use std::ptr::NonNull;
use std::slice;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::graph::{Inputs, MidiMessage, Node, NodeId, Outputs, ProcessContext, Transport};
use crate::sched::dag::{Dag, Pop};

/// Buffer id that always reads as silence; never written.
pub(crate) const SILENCE: u32 = 0;

pub(crate) struct PlannedNode {
    pub(crate) node: Box<dyn Node>,
    pub(crate) in_start: usize,
    pub(crate) in_len: usize,
    pub(crate) out_start: usize,
    pub(crate) out_len: usize,
}

/// A node shared between the threads that run a plan.
pub(crate) struct NodeCell(UnsafeCell<PlannedNode>);

// SAFETY: a node is only touched by the thread that claimed it from the DAG for the current
// cycle, or by the audio thread between cycles (messages), never by two threads at once.
// `Box<dyn Node>` is `Send`, so handing it from thread to thread is allowed.
unsafe impl Sync for NodeCell {}

impl NodeCell {
    pub(crate) fn new(node: PlannedNode) -> Self {
        Self(UnsafeCell::new(node))
    }
}

/// The plan's audio buffers: `count` channels of `stride` samples in one allocation.
pub(crate) struct Buffers {
    data: Box<[UnsafeCell<f32>]>,
    stride: usize,
}

// SAFETY: buffers are written only by the node that owns them as an output for the current
// cycle. `Graph::compile` reuses a buffer only for a node that every earlier user of the buffer
// precedes in the DAG, so a writer never overlaps a reader or another writer of the same buffer.
unsafe impl Sync for Buffers {}

impl Buffers {
    pub(crate) fn new(count: usize, stride: usize) -> Self {
        Self {
            data: (0..count * stride).map(|_| UnsafeCell::new(0.0)).collect(),
            stride,
        }
    }

    pub(crate) fn count(&self) -> usize {
        self.data.len() / self.stride.max(1)
    }

    fn cells(&self, id: u32, frames: usize) -> &[UnsafeCell<f32>] {
        assert!(
            frames <= self.stride,
            "block longer than the plan's buffers"
        );
        let start = id as usize * self.stride;
        &self.data[start..start + frames]
    }

    /// Samples `0..frames` of buffer `id`.
    ///
    /// # Safety
    ///
    /// No thread writes buffer `id` while the returned slice is alive.
    pub(crate) unsafe fn slice(&self, id: u32, frames: usize) -> &[f32] {
        let cells = self.cells(id, frames);
        // SAFETY: `UnsafeCell<f32>` has the layout of `f32`; the caller rules out writers.
        unsafe { slice::from_raw_parts(UnsafeCell::raw_get(cells.as_ptr()), frames) }
    }

    /// Mutable samples `0..frames` of buffer `id`.
    ///
    /// # Safety
    ///
    /// No other reference to buffer `id` exists while the returned slice is alive.
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn slice_mut(&self, id: u32, frames: usize) -> &mut [f32] {
        let cells = self.cells(id, frames);
        // SAFETY: as above, and the caller guarantees exclusive access to this buffer.
        unsafe { slice::from_raw_parts_mut(UnsafeCell::raw_get(cells.as_ptr()), frames) }
    }
}

/// Everything a running plan needs. Lives behind a raw pointer in [`RenderPlan`] so worker
/// threads can hold `&PlanData` while the owning `Box<RenderPlan>` moves between rings.
pub(crate) struct PlanData {
    pub(crate) sample_rate: u32,
    pub(crate) max_block: usize,
    /// Nodes in topological order; `dag` uses the same indices.
    pub(crate) nodes: Box<[NodeCell]>,
    /// Flat list of buffer ids; each node owns an input range and an output range.
    pub(crate) io: Box<[u32]>,
    pub(crate) buffers: Buffers,
    /// `(id, index into nodes)` sorted by id, for allocation-free message routing.
    pub(crate) lookup: Box<[(NodeId, usize)]>,
    /// Latency each node reported at compile time, by node index.
    pub(crate) latencies: Box<[u32]>,
    pub(crate) master: Box<[u32]>,
    /// Latency from the sources to the master output after delay compensation.
    pub(crate) output_latency: u32,
    pub(crate) dag: Dag,
    /// Block parameters of the current cycle, published by `Dag::begin`.
    frames: AtomicUsize,
    playing: AtomicBool,
    position: AtomicU64,
}

impl PlanData {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        sample_rate: u32,
        max_block: usize,
        nodes: Box<[NodeCell]>,
        io: Box<[u32]>,
        buffers: Buffers,
        lookup: Box<[(NodeId, usize)]>,
        latencies: Box<[u32]>,
        master: Box<[u32]>,
        output_latency: u32,
        dag: Dag,
    ) -> Self {
        Self {
            sample_rate,
            max_block,
            nodes,
            io,
            buffers,
            lookup,
            latencies,
            master,
            output_latency,
            dag,
            frames: AtomicUsize::new(0),
            playing: AtomicBool::new(false),
            position: AtomicU64::new(0),
        }
    }

    fn index_of(&self, id: NodeId) -> Option<usize> {
        self.lookup
            .binary_search_by_key(&id, |entry| entry.0)
            .ok()
            .map(|position| self.lookup[position].1)
    }

    /// Starts a cycle of `frames` (`1..=max_block`) frames.
    ///
    /// # Safety
    ///
    /// Audio thread only, and the previous cycle of this plan has finished ([`finish`]
    /// returned), so no node is running.
    ///
    /// [`finish`]: Self::finish
    pub(crate) unsafe fn begin_cycle(&self, frames: usize, transport: Transport) {
        debug_assert!(frames >= 1 && frames <= self.max_block);
        self.frames.store(frames, Ordering::Relaxed);
        self.playing.store(transport.playing, Ordering::Relaxed);
        self.position.store(transport.position, Ordering::Relaxed);
        // SAFETY: forwarded from this function's contract.
        unsafe { self.dag.begin() };
    }

    /// Claims and runs ready nodes until every node of the current cycle is claimed, or until
    /// `stop` returns true while no node is ready (a worker passes its pool's shutdown flag; the
    /// audio thread never stops early). Any number of threads may call this at once; returns how
    /// many nodes this call ran.
    pub(crate) fn work(&self, stop: impl Fn() -> bool) -> u64 {
        let mut ran = 0;
        loop {
            match self.dag.pop() {
                Pop::Node { node, epoch } => {
                    self.run_node(node);
                    self.dag.complete(node, epoch);
                    ran += 1;
                }
                Pop::Empty if stop() => return ran,
                Pop::Empty => std::hint::spin_loop(),
                Pop::Done => return ran,
            }
        }
    }

    /// Waits for nodes other threads are still running. Audio thread only, after [`work`].
    ///
    /// [`work`]: Self::work
    pub(crate) fn finish(&self) {
        self.dag.wait_done();
    }

    fn run_node(&self, index: usize) {
        // The cycle parameters were published before the node could be claimed.
        let frames = self.frames.load(Ordering::Relaxed);
        let transport = Transport {
            playing: self.playing.load(Ordering::Relaxed),
            position: self.position.load(Ordering::Relaxed),
        };
        // SAFETY: the DAG hands node `index` to exactly one thread per cycle, and messages are
        // applied only between cycles, so this is the only reference to the node.
        let planned = unsafe { &mut *self.nodes[index].0.get() };
        let mut ctx = ProcessContext {
            frames,
            sample_rate: self.sample_rate,
            transport,
            inputs: Inputs {
                buffers: &self.buffers,
                ids: &self.io[planned.in_start..planned.in_start + planned.in_len],
                frames,
            },
            outputs: Outputs {
                buffers: &self.buffers,
                ids: &self.io[planned.out_start..planned.out_start + planned.out_len],
                frames,
            },
        };
        planned.node.process(&mut ctx);
    }

    /// Routes a parameter change to a node; false when the node is not in this plan.
    ///
    /// # Safety
    ///
    /// Audio thread only, between cycles.
    pub(crate) unsafe fn set_param(&self, id: NodeId, param: u32, value: f32) -> bool {
        match self.index_of(id) {
            Some(index) => {
                // SAFETY: between cycles no worker touches nodes (caller's contract).
                unsafe { &mut *self.nodes[index].0.get() }
                    .node
                    .set_param(param, value);
                true
            }
            None => false,
        }
    }

    /// Routes a MIDI message to a node; false when the node is not in this plan.
    ///
    /// # Safety
    ///
    /// Audio thread only, between cycles.
    pub(crate) unsafe fn midi(&self, id: NodeId, message: MidiMessage) -> bool {
        match self.index_of(id) {
            Some(index) => {
                // SAFETY: between cycles no worker touches nodes (caller's contract).
                unsafe { &mut *self.nodes[index].0.get() }
                    .node
                    .on_midi(message);
                true
            }
            None => false,
        }
    }

    /// Samples of master channel `channel` from the most recent block.
    ///
    /// # Safety
    ///
    /// Audio thread only, between cycles (no node is writing).
    pub(crate) unsafe fn master_channel(&self, channel: usize, frames: usize) -> Option<&[f32]> {
        self.master
            .get(channel)
            // SAFETY: no cycle is running, so nothing writes the master buffers.
            .map(|&id| unsafe { self.buffers.slice(id, frames) })
    }

    pub(crate) fn master_channels(&self) -> usize {
        self.master.len()
    }
}

/// Everything the audio thread needs for a graph, built by [`Graph::compile`](crate::Graph::compile)
/// on the control thread and then sent to the audio thread over a wait-free ring.
///
/// A plan holds the nodes in dependency order, their dependency graph for the parallel
/// scheduler, the buffer each node channel reads and writes, the delay-compensation nodes the
/// compiler inserted, and the master output channels. Its shape never changes after
/// compilation; the audio thread and its DSP workers only run it. Replacing the graph means
/// compiling a new plan and swapping it in at a cycle boundary; the old plan travels back to the
/// control thread through the retire ring so it is dropped (and freed) off the audio thread,
/// once no worker still looks at it.
///
/// Node state (oscillator phase, filter memory) lives in the plan's nodes, so a recompiled plan
/// starts its nodes from their constructor state unless the control thread carries state over.
pub struct RenderPlan {
    data: NonNull<PlanData>,
}

// SAFETY: `RenderPlan` owns its `PlanData` like a `Box`; `PlanData` holds `Send` nodes and
// plain data, so moving the plan to another thread is sound.
unsafe impl Send for RenderPlan {}

impl RenderPlan {
    pub(crate) fn new(data: PlanData) -> Self {
        Self {
            data: NonNull::from(Box::leak(Box::new(data))),
        }
    }

    /// Shared view of the plan's data.
    pub(crate) fn data(&self) -> &PlanData {
        // SAFETY: `data` came from `Box::leak` in `new` and is freed only in `drop`.
        unsafe { self.data.as_ref() }
    }

    /// Address workers publish as their hazard pointer while they run this plan.
    pub(crate) fn data_ptr(&self) -> *mut PlanData {
        self.data.as_ptr()
    }

    /// Sample rate the plan was compiled for.
    pub fn sample_rate(&self) -> u32 {
        self.data().sample_rate
    }

    /// Largest block the plan's buffers hold.
    pub fn max_block(&self) -> usize {
        self.data().max_block
    }

    /// Number of nodes, including compiler-inserted sums and delay-compensation nodes.
    pub fn node_count(&self) -> usize {
        self.data().nodes.len()
    }

    /// Number of audio buffers (channels) the plan owns, including the silence buffer.
    pub fn buffer_count(&self) -> usize {
        self.data().buffers.count()
    }

    /// Number of master output channels.
    pub fn master_channels(&self) -> usize {
        self.data().master_channels()
    }

    /// Latency in samples reported by a node, if it is in the plan.
    pub fn latency_of(&self, id: NodeId) -> Option<u32> {
        let data = self.data();
        data.index_of(id).map(|index| data.latencies[index])
    }

    /// Latency in samples from the graph's sources to the master output after delay
    /// compensation: the longest latency path into the master. Later stages offset clip and
    /// automation schedules by it.
    pub fn output_latency(&self) -> u32 {
        self.data().output_latency
    }
}

impl Drop for RenderPlan {
    fn drop(&mut self) {
        // SAFETY: `data` came from `Box::leak` in `new` and nothing else frees it. Plans that
        // were live are dropped only after the scheduler's hazard check (`ControlLink`), or
        // after the worker pool has shut down (`AudioCore`).
        drop(unsafe { Box::from_raw(self.data.as_ptr()) });
    }
}
