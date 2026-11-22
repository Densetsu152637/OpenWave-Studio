//! The audio-thread side of the engine ([`AudioCore`]) and the control-thread side
//! ([`ControlLink`]), joined by wait-free rings (architecture section 4).

// The audio thread routes messages into the live plan between cycles; see `PlanData`.
#![allow(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::thread;

use assert_no_alloc::assert_no_alloc;

use super::NodeId;
use crate::plan::RenderPlan;
use crate::rings::{ProducerRing, RingSender, producer_ring};
use crate::sched::AbortOnPanic;
use crate::sched::denormal::DenormalGuard;
use crate::sched::pool::{Pool, PoolConfig, Shared};
use crate::transport::{Timeline, TimelineSwapError, Transport, TransportCommand, TransportState};
use crate::workgroup::AudioWorkgroup;

/// Capacity of the timeline ring (and, plus two, of its retire ring).
const TIMELINE_QUEUE: usize = 4;

/// Largest worker count [`CoreBuilder::workers`] accepts.
pub const MAX_WORKERS: usize = 64;

/// The architecture's default worker count: physical cores minus one (the callback thread is
/// the remaining DSP thread), at least 1.
pub fn default_worker_count() -> usize {
    num_cpus::get_physical()
        .saturating_sub(1)
        .clamp(1, MAX_WORKERS)
}

/// A short MIDI message (status byte plus up to two data bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidiMessage {
    /// Status byte.
    pub status: u8,
    /// First data byte.
    pub data1: u8,
    /// Second data byte.
    pub data2: u8,
}

/// Fixed-size, `Copy` message from a producer thread to the audio thread. Applied at the start
/// of the next cycle, in ring order. Anything that needs more than this (new nodes, topology)
/// travels as a new plan instead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Message {
    /// Set a node parameter. Dropped (and counted in [`Telemetry::unrouted`]) when the node is
    /// not in the current plan, so send parameters for a new node only after its plan is live.
    SetParam {
        /// Target node.
        node: NodeId,
        /// Node-defined parameter index.
        param: u32,
        /// New value.
        value: f32,
    },
    /// Change the transport.
    Transport(TransportCommand),
    /// Deliver MIDI to a node.
    Midi {
        /// Target node.
        node: NodeId,
        /// The message.
        message: MidiMessage,
        /// Frames after the start of the cycle at which the event belongs. Reserved for
        /// sample-accurate delivery: today every message is applied at the start of the cycle
        /// that drains it and the offset is carried but not yet used.
        offset: u32,
    },
}

/// Counters the audio thread and the DSP workers publish; read from any thread.
#[derive(Debug)]
pub struct Telemetry {
    cycles: AtomicU64,
    plan_swaps: AtomicU64,
    unrouted: AtomicU64,
    position: AtomicU64,
    /// Nodes run per DSP thread: index 0 is the callback thread, `1..` the workers.
    nodes_run: Box<[AtomicU64]>,
    realtime_workers: AtomicU32,
    unpromoted_workers: AtomicU32,
    workgroup_workers: AtomicU32,
}

impl Telemetry {
    fn new(threads: usize) -> Self {
        Self {
            cycles: AtomicU64::new(0),
            plan_swaps: AtomicU64::new(0),
            unrouted: AtomicU64::new(0),
            position: AtomicU64::new(0),
            nodes_run: (0..threads).map(|_| AtomicU64::new(0)).collect(),
            realtime_workers: AtomicU32::new(0),
            unpromoted_workers: AtomicU32::new(0),
            workgroup_workers: AtomicU32::new(0),
        }
    }

    pub(crate) fn count_nodes(&self, thread: usize, nodes: u64) {
        if let Some(counter) = self.nodes_run.get(thread) {
            counter.fetch_add(nodes, Ordering::Relaxed);
        }
    }

    pub(crate) fn count_realtime_worker(&self) {
        self.realtime_workers.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count_unpromoted_worker(&self) {
        self.unpromoted_workers.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count_workgroup_worker(&self) {
        self.workgroup_workers.fetch_add(1, Ordering::Relaxed);
    }

    /// DSP threads the core was built with: the callback thread plus the requested workers.
    pub fn threads(&self) -> usize {
        self.nodes_run.len()
    }

    /// Nodes run so far by DSP thread `thread` (0 is the callback thread, `1..` the workers);
    /// 0 for an unknown thread. Shows how work spreads across the pool.
    pub fn nodes_run(&self, thread: usize) -> u64 {
        self.nodes_run
            .get(thread)
            .map_or(0, |counter| counter.load(Ordering::Relaxed))
    }

    /// Workers that obtained real-time priority from `audio_thread_priority`. With
    /// [`CoreBuilder::realtime_workers`] these are the workers that take part in cycles.
    pub fn realtime_workers(&self) -> u32 {
        self.realtime_workers.load(Ordering::Relaxed)
    }

    /// Workers that were asked to become real-time, failed, and exited without ever running a
    /// node (the callback thread and the promoted workers do their share).
    pub fn unpromoted_workers(&self) -> u32 {
        self.unpromoted_workers.load(Ordering::Relaxed)
    }

    /// Workers that joined the macOS audio workgroup.
    pub fn workgroup_workers(&self) -> u32 {
        self.workgroup_workers.load(Ordering::Relaxed)
    }

    /// Engine cycles (device callbacks) run so far.
    pub fn cycles(&self) -> u64 {
        self.cycles.load(Ordering::Relaxed)
    }
    /// Plans swapped in so far.
    pub fn plan_swaps(&self) -> u64 {
        self.plan_swaps.load(Ordering::Relaxed)
    }
    /// Messages dropped because their node is not in the current plan.
    pub fn unrouted(&self) -> u64 {
        self.unrouted.load(Ordering::Relaxed)
    }
    /// Transport position in samples at the end of the last cycle (for the UI's position
    /// readout; a relaxed atomic the audio thread stores once per cycle).
    pub fn transport_position(&self) -> u64 {
        self.position.load(Ordering::Relaxed)
    }
}

/// The plan ring was full; the plan is handed back so the caller can retry.
pub struct SwapError(pub Box<RenderPlan>);

impl std::fmt::Debug for SwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SwapError(plan ring full)")
    }
}

impl std::fmt::Display for SwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the plan ring is full")
    }
}

impl std::error::Error for SwapError {}

/// Assembles an [`AudioCore`] and its [`ControlLink`], registering extra producer rings and
/// configuring the DSP worker pool.
pub struct CoreBuilder {
    plan_queue: usize,
    retire_queue: Option<usize>,
    control_ring: usize,
    rings: Vec<ProducerRing<Message>>,
    pool: PoolConfig,
    timeline: Timeline,
}

impl CoreBuilder {
    /// A builder with a plan queue of 8 plans, a control ring of 1024 messages and no DSP
    /// workers (the callback thread runs every node).
    pub fn new() -> Self {
        Self {
            plan_queue: 8,
            retire_queue: None,
            control_ring: 1024,
            rings: Vec::new(),
            pool: PoolConfig::default(),
            timeline: Timeline::default(),
        }
    }

    /// Number of DSP worker threads that help the callback thread run each cycle (the user's
    /// setting; [`default_worker_count`] is the architecture default). 0 runs everything on the
    /// callback thread. Clamped to [`MAX_WORKERS`]. Output is bit-identical for every count.
    pub fn workers(mut self, count: usize) -> Self {
        self.pool.workers = count.min(MAX_WORKERS);
        self
    }

    /// Promotes the workers to real-time priority with `audio_thread_priority`, sized for
    /// device blocks of `block_frames` (0 for a default) at `sample_rate`. Without this the
    /// workers keep normal priority, which suits offline rendering. A worker whose promotion
    /// fails exits without taking part in any cycle, so a real-time cycle never waits on a
    /// normal-priority thread ([`Telemetry::realtime_workers`] counts the participants,
    /// [`Telemetry::unpromoted_workers`] the rest).
    pub fn realtime_workers(mut self, sample_rate: u32, block_frames: u32) -> Self {
        self.pool.realtime = Some((block_frames, sample_rate));
        self
    }

    /// Replaces the real-time promotion (tests simulate failures with it).
    #[cfg(test)]
    pub(crate) fn promoter(mut self, promote: crate::sched::pool::Promote) -> Self {
        self.pool.promote = promote;
        self
    }

    /// Makes every worker join `workgroup` (macOS audio workgroup of the output device) for
    /// its lifetime. [`Telemetry::workgroup_workers`] counts successful joins.
    pub fn audio_workgroup(mut self, workgroup: AudioWorkgroup) -> Self {
        self.pool.workgroup = Some(Arc::new(workgroup));
        self
    }

    /// Capacity of the plan ring (and the retire ring behind it).
    pub fn plan_queue(mut self, capacity: usize) -> Self {
        self.plan_queue = capacity.max(1);
        self
    }

    /// Capacity of the retire ring that returns swapped-out plans (default: plan queue + 2).
    /// Mainly for tests of the back-pressure path; a full retire ring delays swaps, it never
    /// makes the audio thread free a plan.
    pub fn retire_queue(mut self, capacity: usize) -> Self {
        self.retire_queue = Some(capacity.max(1));
        self
    }

    /// The timeline (tempo and time-signature maps) the core starts with; the default is
    /// 120 beats per minute in 4/4. Replace it later with
    /// [`ControlLink::swap_timeline`].
    pub fn timeline(mut self, timeline: Timeline) -> Self {
        self.timeline = timeline;
        self
    }

    /// Capacity of the control thread's message ring.
    pub fn control_ring(mut self, capacity: usize) -> Self {
        self.control_ring = capacity.max(1);
        self
    }

    /// Registers another producer (for example one MIDI input thread) and returns its sender.
    /// Call before [`build`](Self::build); the audio thread drains the ring every cycle.
    pub fn producer_ring(&mut self, capacity: usize) -> RingSender<Message> {
        let (sender, ring) = producer_ring(capacity);
        self.rings.push(ring);
        sender
    }

    /// Creates both halves and starts the worker threads. The control thread keeps the
    /// [`ControlLink`]; the audio callback owns the [`AudioCore`]. Dropping the core stops the
    /// workers.
    pub fn build(self) -> (ControlLink, AudioCore) {
        let (plan_tx, plan_rx) = producer_ring::<Box<RenderPlan>>(self.plan_queue);
        // One more slot than plans in flight, so the audio thread can always retire.
        let (retire_tx, retire_rx) =
            producer_ring::<Box<RenderPlan>>(self.retire_queue.unwrap_or(self.plan_queue + 2));
        let (control_tx, control_rx) = producer_ring::<Message>(self.control_ring);
        let (timeline_tx, timeline_rx) = producer_ring::<Box<Timeline>>(TIMELINE_QUEUE);
        let (timeline_retire_tx, timeline_retire_rx) =
            producer_ring::<Box<Timeline>>(TIMELINE_QUEUE + 2);
        let telemetry = Arc::new(Telemetry::new(self.pool.workers + 1));
        let pool = Pool::spawn(&self.pool, &telemetry);
        let mut rings = Vec::with_capacity(self.rings.len() + 1);
        rings.push(control_rx);
        rings.extend(self.rings);
        (
            ControlLink {
                plans: plan_tx,
                retired: retire_rx,
                messages: control_tx,
                timelines: timeline_tx,
                retired_timelines: timeline_retire_rx,
                telemetry: Arc::clone(&telemetry),
                sched: Arc::clone(pool.shared()),
            },
            AudioCore {
                plans: plan_rx,
                retire: retire_tx,
                rings,
                current: None,
                pending_retire: None,
                timelines: timeline_rx,
                retire_timelines: timeline_retire_tx,
                timeline: Box::new(self.timeline),
                pending_retire_timeline: None,
                transport: TransportState::new(),
                telemetry,
                pool,
            },
        )
    }
}

impl Default for CoreBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Control-thread end: sends plans and messages, collects retired plans.
pub struct ControlLink {
    plans: RingSender<Box<RenderPlan>>,
    retired: ProducerRing<Box<RenderPlan>>,
    messages: RingSender<Message>,
    timelines: RingSender<Box<Timeline>>,
    retired_timelines: ProducerRing<Box<Timeline>>,
    telemetry: Arc<Telemetry>,
    sched: Arc<Shared>,
}

impl ControlLink {
    /// Queues a plan for the audio thread to swap in at its next cycle boundary. Also frees any
    /// plans the audio thread has already retired. Returns the plan if the ring is full.
    pub fn swap_plan(&mut self, plan: RenderPlan) -> Result<(), SwapError> {
        self.collect_retired();
        self.plans.try_push(Box::new(plan)).map_err(SwapError)
    }

    /// Queues a timeline (tempo and time-signature maps) for the audio thread to swap in at its
    /// next cycle boundary. The transport keeps its musical position (the beat) across the
    /// swap, and its loop and punch ranges follow the new tempo. Returns the timeline if the
    /// ring is full.
    pub fn swap_timeline(&mut self, timeline: Timeline) -> Result<(), TimelineSwapError> {
        self.collect_retired();
        self.timelines
            .try_push(Box::new(timeline))
            .map_err(TimelineSwapError)
    }

    /// Drops timelines and plans the audio thread has retired (this is where their memory is
    /// freed) and returns how many plans. A DSP worker may still be leaving a just-retired plan; this waits
    /// (yielding) until none is, which takes at most a few instructions of the worker's time.
    pub fn collect_retired(&mut self) -> usize {
        while let Some(timeline) = self.retired_timelines.pop() {
            drop(timeline);
        }
        let mut dropped = 0;
        while let Some(plan) = self.retired.pop() {
            while self.sched.in_use(plan.data_ptr()) {
                thread::yield_now();
            }
            drop(plan);
            dropped += 1;
        }
        dropped
    }

    /// Sends a control message; false when the ring was full (counted in
    /// [`message_overflow`](Self::message_overflow)).
    pub fn send(&mut self, message: Message) -> bool {
        self.messages.push(message)
    }

    /// Like [`send`](Self::send) but hands the message back when the ring is full, without
    /// counting an overflow, so the caller can retry or report failure.
    pub fn try_send(&mut self, message: Message) -> Result<(), Message> {
        self.messages.try_push(message)
    }

    /// Messages dropped because the control ring was full.
    pub fn message_overflow(&self) -> u64 {
        self.messages.overflow_count()
    }

    /// Shared audio-thread counters.
    pub fn telemetry(&self) -> &Arc<Telemetry> {
        &self.telemetry
    }
}

/// Audio-thread end: owns the current plan and the DSP worker pool, and runs one engine cycle
/// per device buffer.
///
/// [`process`](Self::process) is allocation-free and lock-free and runs under `assert_no_alloc`
/// (active in debug and test builds when the binary installs `assert_no_alloc::AllocDisabler`
/// as its global allocator), as does every worker. The callback thread takes part in running
/// the nodes; it waits only for nodes a worker has already started (see [`crate::sched`]).
pub struct AudioCore {
    plans: ProducerRing<Box<RenderPlan>>,
    retire: RingSender<Box<RenderPlan>>,
    rings: Vec<ProducerRing<Message>>,
    current: Option<Box<RenderPlan>>,
    pending_retire: Option<Box<RenderPlan>>,
    timelines: ProducerRing<Box<Timeline>>,
    retire_timelines: RingSender<Box<Timeline>>,
    timeline: Box<Timeline>,
    pending_retire_timeline: Option<Box<Timeline>>,
    transport: TransportState,
    telemetry: Arc<Telemetry>,
    pool: Pool,
}

impl AudioCore {
    /// Number of producer rings drained each cycle (the control ring plus registered ones).
    pub fn ring_count(&self) -> usize {
        self.rings.len()
    }

    /// Total messages dropped across all producer rings.
    pub fn overflow_total(&self) -> u64 {
        self.rings.iter().map(ProducerRing::overflow_count).sum()
    }

    /// Current transport state.
    pub fn transport(&self) -> Transport {
        self.transport.summary()
    }

    /// Number of running DSP worker threads (the callback thread is not counted).
    pub fn workers(&self) -> usize {
        self.pool.workers()
    }

    /// Runs one cycle: swap in a pending plan, apply queued messages, render into the
    /// interleaved device buffer `out` with `channels` channels. A device buffer longer than the
    /// plan's block is rendered in chunks. With no plan the output is silence. Non-finite
    /// samples are written as 0.0 so a faulty node cannot send NaN or infinity to the device.
    ///
    /// `input` is the device's interleaved input audio for this cycle (same frame count as
    /// `out`), or `None` when there is none. It is accepted for the stage 9 mock backend and
    /// stage 14 recording and is currently ignored.
    pub fn process(&mut self, out: &mut [f32], channels: usize, input: Option<&[f32]>) {
        let _ = input;
        // A panicking node aborts the process rather than leaving workers and the device
        // callback in an inconsistent cycle.
        let _abort = AbortOnPanic;
        assert_no_alloc(|| {
            let _denormals = DenormalGuard::new();
            self.cycle(out, channels);
        });
    }

    fn cycle(&mut self, out: &mut [f32], channels: usize) {
        self.telemetry.cycles.fetch_add(1, Ordering::Relaxed);
        self.flush_retired();
        self.swap_pending_plan();
        self.swap_pending_timeline();
        self.drain_rings();
        out.fill(0.0);
        let channels = channels.max(1);
        let total = out.len() / channels;
        let mut done = 0;
        while done < total {
            let Some(plan) = self.current.as_deref() else {
                // No plan: silence, but keep the transport moving.
                let block = self.transport.begin_block(total - done);
                self.transport.end_block(&block, &self.timeline);
                break;
            };
            let data = plan.data();
            let frames = (total - done).min(data.max_block);
            let block = self.transport.begin_block(frames);
            // SAFETY: this is the audio thread and the previous cycle of this plan finished;
            // `self.timeline` is replaced only between cycles, so it outlives this one.
            unsafe { data.begin_cycle(frames, block, &self.timeline) };
            self.pool.wake();
            let ran = data.work(|| false);
            data.finish();
            self.telemetry.count_nodes(0, ran);
            for channel in 0..channels {
                let master_channel = if data.master_channels() == 1 {
                    0
                } else {
                    channel
                };
                // SAFETY: the cycle finished, so no node writes the master buffers.
                if let Some(source) = unsafe { data.master_channel(master_channel, frames) } {
                    for (frame, sample) in source.iter().enumerate() {
                        out[(done + frame) * channels + channel] =
                            if sample.is_finite() { *sample } else { 0.0 };
                    }
                }
            }
            done += frames;
            self.transport.end_block(&block, &self.timeline);
        }
        self.telemetry
            .position
            .store(self.transport.position(), Ordering::Relaxed);
    }

    /// Hands a previously swapped-out plan back to the control thread; keeps it for the next
    /// cycle when the retire ring is full, so the audio thread never frees it.
    fn flush_retired(&mut self) {
        if let Some(old) = self.pending_retire.take()
            && let Err(old) = self.retire.try_push(old)
        {
            self.pending_retire = Some(old);
        }
    }

    fn swap_pending_plan(&mut self) {
        if self.pending_retire.is_some() {
            return;
        }
        if let Some(new) = self.plans.pop() {
            // Workers move to the new plan before the old one can reach the control thread.
            self.pool.publish(new.data_ptr());
            self.transport
                .set_sample_rate(new.data().sample_rate, &self.timeline);
            self.pending_retire = self.current.replace(new);
            self.telemetry.plan_swaps.fetch_add(1, Ordering::Relaxed);
            self.flush_retired();
        }
    }

    /// Swaps in queued timelines and hands the old ones back; with the retire ring full the
    /// swap waits, so the audio thread never frees a timeline.
    fn swap_pending_timeline(&mut self) {
        loop {
            if let Some(old) = self.pending_retire_timeline.take()
                && let Err(old) = self.retire_timelines.try_push(old)
            {
                self.pending_retire_timeline = Some(old);
                return;
            }
            let Some(new) = self.timelines.pop() else {
                return;
            };
            let old = std::mem::replace(&mut self.timeline, new);
            self.transport.timeline_changed(&old, &self.timeline);
            self.pending_retire_timeline = Some(old);
        }
    }

    fn drain_rings(&mut self) {
        let Self {
            rings,
            current,
            transport,
            telemetry,
            timeline,
            ..
        } = self;
        for ring in rings.iter_mut() {
            while let Some(message) = ring.pop() {
                let routed = match message {
                    // SAFETY (both arms): audio thread, between cycles.
                    Message::SetParam { node, param, value } => current
                        .as_deref()
                        .is_some_and(|plan| unsafe { plan.data().set_param(node, param, value) }),
                    Message::Midi { node, message, .. } => current
                        .as_deref()
                        .is_some_and(|plan| unsafe { plan.data().midi(node, message) }),
                    Message::Transport(command) => {
                        transport.apply(command, timeline);
                        true
                    }
                };
                if !routed {
                    telemetry.unrouted.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

impl Drop for AudioCore {
    fn drop(&mut self) {
        // Stop the workers before the plans they may reference are dropped with the fields.
        self.pool.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::graph::Graph;
    use crate::graph::nodes::{GainNode, MasterNode, OscillatorNode};
    use crate::sched::pool::{promote_never, promote_noop};

    /// 32 parallel chains of an oscillator and four gains.
    fn wide_plan() -> RenderPlan {
        let mut graph = Graph::new();
        let master = graph.add_node(Box::new(MasterNode::new()));
        for track in 0..32 {
            let mut tail = graph.add_node(Box::new(OscillatorNode::new(100.0, 0.01)));
            for _ in 0..4 {
                let gain = graph.add_node(Box::new(GainNode::new(1, 0.99)));
                graph.connect(tail, 0, gain, 0).unwrap();
                tail = gain;
            }
            graph.connect(tail, 0, master, track % 2).unwrap();
        }
        graph.set_master(master).unwrap();
        graph.compile(48_000, 512).unwrap()
    }

    fn worker_nodes(telemetry: &Telemetry) -> u64 {
        (1..telemetry.threads())
            .map(|thread| telemetry.nodes_run(thread))
            .sum()
    }

    /// Builds a 4-worker real-time core with `promote`, waits until every worker has reported
    /// its promotion, then runs cycles until `done` or 30 s.
    fn run(
        promote: crate::sched::pool::Promote,
        done: impl Fn(&Telemetry) -> bool,
    ) -> Arc<Telemetry> {
        let (mut link, mut core) = CoreBuilder::new()
            .workers(4)
            .realtime_workers(48_000, 512)
            .promoter(promote)
            .build();
        link.swap_plan(wide_plan()).unwrap();
        let telemetry = Arc::clone(link.telemetry());
        let deadline = Instant::now() + Duration::from_secs(30);
        while telemetry.realtime_workers() + telemetry.unpromoted_workers() < 4 {
            assert!(Instant::now() < deadline, "workers never reported");
            thread::yield_now();
        }
        let mut buffer = vec![0.0_f32; 512 * 2];
        let mut cycles = 0;
        while cycles < 500 || (!done(&telemetry) && Instant::now() < deadline) {
            core.process(&mut buffer, 2, None);
            cycles += 1;
        }
        telemetry
    }

    #[test]
    fn workers_that_fail_real_time_promotion_never_run_nodes() {
        let telemetry = run(promote_never, |_| true);
        assert_eq!(telemetry.unpromoted_workers(), 4);
        assert_eq!(telemetry.realtime_workers(), 0);
        assert_eq!(worker_nodes(&telemetry), 0);
        assert!(
            telemetry.nodes_run(0) > 0,
            "the callback thread ran everything"
        );
    }

    #[test]
    fn promoted_workers_take_part() {
        let telemetry = run(promote_noop, |t| worker_nodes(t) > 0);
        assert_eq!(telemetry.realtime_workers(), 4);
        assert_eq!(telemetry.unpromoted_workers(), 0);
        assert!(
            worker_nodes(&telemetry) > 0,
            "no promoted worker ran a node"
        );
    }
}
