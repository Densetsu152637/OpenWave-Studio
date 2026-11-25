//! The audio-thread side of the engine ([`AudioCore`]) and the control-thread side
//! ([`ControlLink`]), joined by wait-free rings (architecture section 4).

// The audio thread routes messages into the live plan between cycles; see `PlanData`.
#![allow(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::thread;

use assert_no_alloc::assert_no_alloc;

use super::NodeId;
use crate::plan::RenderPlan;
use crate::preview::Preview;
use crate::record::{PendingMidi, RecordSet};
use crate::rings::{ProducerRing, RingSender, producer_ring};
use crate::sched::AbortOnPanic;
use crate::sched::denormal::DenormalGuard;
use crate::sched::pool::{Pool, PoolConfig, Shared};
use crate::transport::{
    BlockTiming, Timeline, TimelineSwapError, Transport, TransportCommand, TransportState,
};
use crate::workgroup::AudioWorkgroup;

/// Capacity of the timeline ring (and, plus two, of its retire ring).
const TIMELINE_QUEUE: usize = 4;

/// Capacity of the record-set ring (and, plus two, of its retire ring): at most one recording is
/// live, so a few slots are plenty.
const RECORD_QUEUE: usize = 2;

/// Most live MIDI messages the core holds between receiving them and their block.
const MIDI_PENDING: usize = 4096;

/// How far (in seconds) a live MIDI message's timestamp may lie before and after the start of the
/// cycle that receives it; a stamp further away is placed at the nearest limit (a bad clock must
/// not park a message for ever).
const MIDI_WINDOW_PAST: i64 = 2;
const MIDI_WINDOW_FUTURE_MS: i64 = 500;

/// The clock the audio thread uses to place the timestamps of live MIDI input.
///
/// A message carries the time it was received. The core reads the clock at the start of every
/// cycle and places a message at the stream frame `cycle_start_frame + (stamp - clock) * rate`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MidiClock {
    /// The wall clock the MIDI layer stamps messages with ([`ow_io::now_micros`]). Placement is
    /// as accurate as the handler thread's wake-up after the hardware event.
    #[default]
    Wall,
    /// Time is the stream's own frame counter divided by the sample rate, so a message stamped
    /// `frame * 1e6 / rate` lands on exactly that stream frame however late it is delivered.
    /// For tests driven through the mock backend.
    StreamFrames,
}

/// Capacity of the preview ring (and, plus two, of its retire ring): one preview plays at a
/// time and a new one replaces it.
const PREVIEW_QUEUE: usize = 2;

/// Sample rate assumed for the block timing of a cycle that has no plan (only the frame
/// positions matter to the record tap, not the rate).
const NO_PLAN_RATE: u32 = 48_000;

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
    /// Finish the recording in progress: the audio thread pushes no more frames to it and hands
    /// it back to the control thread (see [`crate::record`]). Harmless when none is running.
    EndRecording,
    /// Live MIDI input from a (non control-surface) port: reaches the instruments of armed
    /// tracks and the record tap, placed on the block by `timestamp_us` (see [`MidiClock`]).
    MidiIn {
        /// The message.
        message: MidiMessage,
        /// Time the MIDI layer received it, in microseconds on [`ow_io::now_micros`]'s clock
        /// (or the test's, with [`MidiClock::StreamFrames`]).
        timestamp_us: u64,
    },
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
    /// `ow_ipc::TRANSPORT_*` bits at the end of the last cycle.
    transport_flags: AtomicU8,
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
            transport_flags: AtomicU8::new(0),
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
    /// What the transport was doing at the end of the last cycle, as `ow_ipc::TRANSPORT_*` bits.
    pub fn transport_flags(&self) -> u8 {
        self.transport_flags.load(Ordering::Relaxed)
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
    clock: MidiClock,
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
            clock: MidiClock::default(),
        }
    }

    /// The clock that places live MIDI timestamps (default [`MidiClock::Wall`]).
    pub fn midi_clock(mut self, clock: MidiClock) -> Self {
        self.clock = clock;
        self
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
        let (record_tx, record_rx) = producer_ring::<Box<RecordSet>>(RECORD_QUEUE);
        let (record_retire_tx, record_retire_rx) =
            producer_ring::<Box<RecordSet>>(RECORD_QUEUE + 2);
        let (preview_tx, preview_rx) = producer_ring::<Box<Preview>>(PREVIEW_QUEUE);
        let (preview_retire_tx, preview_retire_rx) =
            producer_ring::<Box<Preview>>(PREVIEW_QUEUE + 2);
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
                records: record_tx,
                retired_records: record_retire_rx,
                previews: preview_tx,
                retired_previews: preview_retire_rx,
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
                records: record_rx,
                retire_records: record_retire_tx,
                record: None,
                pending_retire_record: None,
                previews: preview_rx,
                retire_previews: preview_retire_tx,
                preview: None,
                fading_preview: None,
                pending_retire_preview: None,
                transport: TransportState::new(),
                telemetry,
                pool,
                clock: self.clock,
                stream_frames: 0,
                cycle_us: 0,
                midi_pending: Vec::with_capacity(MIDI_PENDING),
                midi_seq: 0,
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
    records: RingSender<Box<RecordSet>>,
    retired_records: ProducerRing<Box<RecordSet>>,
    previews: RingSender<Box<Preview>>,
    retired_previews: ProducerRing<Box<Preview>>,
    telemetry: Arc<Telemetry>,
    sched: Arc<Shared>,
}

/// The preview ring was full; the preview is handed back so the caller can retry.
pub struct PreviewSwapError(pub Box<Preview>);

impl std::fmt::Debug for PreviewSwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreviewSwapError(preview ring full)")
    }
}

impl std::fmt::Display for PreviewSwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the preview ring is full")
    }
}

impl std::error::Error for PreviewSwapError {}

/// The record ring was full; the set is handed back so the caller can retry.
pub struct RecordSwapError(pub Box<RecordSet>);

impl std::fmt::Debug for RecordSwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecordSwapError(record ring full)")
    }
}

impl std::fmt::Display for RecordSwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the record ring is full")
    }
}

impl std::error::Error for RecordSwapError {}

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

    /// Queues a recording for the audio thread to tap the input into from its next cycle on (it
    /// records while the transport is recording). Collects finished recordings first. Returns
    /// the set if the ring is full. End it with [`Message::EndRecording`].
    pub fn swap_record(&mut self, set: RecordSet) -> Result<(), RecordSwapError> {
        self.collect_retired();
        self.records
            .try_push(Box::new(set))
            .map_err(RecordSwapError)
    }

    /// Queues a preview for the audio thread to mix into the output from its next cycle on; it
    /// replaces a preview that is still playing. Returns the preview if the ring is full.
    pub fn swap_preview(&mut self, preview: Preview) -> Result<(), PreviewSwapError> {
        self.collect_retired();
        self.previews
            .try_push(Box::new(preview))
            .map_err(PreviewSwapError)
    }

    /// Drops timelines, recordings and plans the audio thread has retired (this is where their
    /// memory is freed) and returns how many plans. A DSP worker may still be leaving a just-retired plan; this waits
    /// (yielding) until none is, which takes at most a few instructions of the worker's time.
    pub fn collect_retired(&mut self) -> usize {
        while let Some(timeline) = self.retired_timelines.pop() {
            drop(timeline);
        }
        while let Some(set) = self.retired_records.pop() {
            drop(set);
        }
        while let Some(preview) = self.retired_previews.pop() {
            drop(preview);
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
    records: ProducerRing<Box<RecordSet>>,
    retire_records: RingSender<Box<RecordSet>>,
    /// The recording the input tap feeds, until it ends.
    record: Option<Box<RecordSet>>,
    /// A recording that ended and waits for room in the retire ring.
    pending_retire_record: Option<Box<RecordSet>>,
    previews: ProducerRing<Box<Preview>>,
    retire_previews: RingSender<Box<Preview>>,
    /// The preview being mixed into the output, until it ends.
    preview: Option<Box<Preview>>,
    /// The preview a newer one replaced, fading out.
    fading_preview: Option<Box<Preview>>,
    /// A preview that ended and waits for room in the retire ring.
    pending_retire_preview: Option<Box<Preview>>,
    transport: TransportState,
    telemetry: Arc<Telemetry>,
    pool: Pool,
    clock: MidiClock,
    /// Frames rendered since the core was built.
    stream_frames: u64,
    /// The clock's reading at the start of the cycle.
    cycle_us: u64,
    /// Live MIDI messages placed on the stream frame counter, sorted by frame, until their block.
    midi_pending: Vec<PendingMidi>,
    midi_seq: u64,
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
    /// `out`), or `None` when there is none. While the transport is recording and a
    /// [`RecordSet`] is installed, the input is pushed into its takes (see [`crate::record`]),
    /// and the plan's input monitors play the channels they route; otherwise it is ignored.
    pub fn process(&mut self, out: &mut [f32], channels: usize, input: Option<&[f32]>) {
        // A panicking node aborts the process rather than leaving workers and the device
        // callback in an inconsistent cycle.
        let _abort = AbortOnPanic;
        assert_no_alloc(|| {
            let _denormals = DenormalGuard::new();
            self.cycle(out, channels, input);
        });
    }

    fn cycle(&mut self, out: &mut [f32], channels: usize, input: Option<&[f32]>) {
        self.telemetry.cycles.fetch_add(1, Ordering::Relaxed);
        self.flush_retired();
        self.swap_pending_plan();
        self.swap_pending_timeline();
        self.swap_pending_record();
        let rate = self
            .current
            .as_deref()
            .map_or(NO_PLAN_RATE, |plan| plan.data().sample_rate);
        self.cycle_us = match self.clock {
            MidiClock::Wall => ow_io::now_micros(),
            MidiClock::StreamFrames => {
                (self.stream_frames as f64 * 1e6 / f64::from(rate.max(1))) as u64
            }
        };
        self.swap_pending_preview();
        self.drain_rings();
        if self.midi_pending.len() > 1 {
            self.midi_pending
                .sort_unstable_by_key(|event| (event.frame, event.seq));
        }
        self.flush_record();
        out.fill(0.0);
        let channels = channels.max(1);
        let total = out.len() / channels;
        // The input with its channels per frame; 0 channels marks a buffer that is not a whole
        // number of frames (counted as skipped when recording, not monitored).
        let input = input.map(|buffer| {
            let per_frame = if total > 0 && buffer.len() % total == 0 {
                buffer.len() / total
            } else {
                0
            };
            (buffer, per_frame)
        });
        let chunk_of = |done: usize, frames: usize| {
            input.map(|(buffer, per_frame)| {
                if per_frame == 0 {
                    (buffer, 0)
                } else {
                    (
                        &buffer[done * per_frame..(done + frames) * per_frame],
                        per_frame,
                    )
                }
            })
        };
        let mut done = 0;
        while done < total {
            let Some(plan) = self.current.as_deref() else {
                // No plan: silence, but keep the transport moving (and the recording running).
                let frames = total - done;
                let block = self.transport.begin_block(frames);
                let chunk_start = self.stream_frames;
                let ready = self
                    .midi_pending
                    .partition_point(|event| event.frame < (chunk_start + frames as u64) as i64);
                if let Some(set) = self.record.as_deref_mut() {
                    let timing = BlockTiming::new(block, &self.timeline, NO_PLAN_RATE);
                    set.tap(
                        timing,
                        chunk_of(done, frames),
                        (chunk_start, &self.midi_pending[..ready]),
                    );
                }
                self.midi_pending.drain(..ready);
                self.stream_frames += frames as u64;
                self.transport.end_block(&block, &self.timeline);
                break;
            };
            let data = plan.data();
            let frames = (total - done).min(data.max_block);
            let block = self.transport.begin_block(frames);
            let chunk = chunk_of(done, frames);
            let chunk_start = self.stream_frames;
            // The live MIDI messages of this chunk: to the record tap, and to the nodes that
            // play them (between cycles, like every message into the plan).
            let ready = self
                .midi_pending
                .partition_point(|event| event.frame < (chunk_start + frames as u64) as i64);
            if let Some(set) = self.record.as_deref_mut() {
                let timing = BlockTiming::new(block, &self.timeline, data.sample_rate);
                set.tap(timing, chunk, (chunk_start, &self.midi_pending[..ready]));
            }
            for event in &self.midi_pending[..ready] {
                let offset = (event.frame - chunk_start as i64).clamp(0, frames as i64 - 1);
                // SAFETY: audio thread, between cycles (no node runs yet).
                unsafe { data.live_midi(offset as u32, event.message) };
            }
            self.midi_pending.drain(..ready);
            // SAFETY: this is the audio thread and the previous cycle of this plan finished
            // (no node runs); `self.timeline` is replaced only between cycles, so it outlives
            // this one.
            unsafe {
                data.feed_input(chunk, frames);
                data.begin_cycle(frames, block, &self.timeline);
            }
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
            self.stream_frames += frames as u64;
        }
        if let Some(preview) = self.preview.as_deref_mut() {
            preview.mix(out, channels);
        }
        if let Some(preview) = self.fading_preview.as_deref_mut() {
            preview.mix(out, channels);
        }
        self.flush_preview();
        self.telemetry
            .position
            .store(self.transport.position(), Ordering::Relaxed);
        let state = self.transport.summary();
        let mut flags = 0;
        for (on, bit) in [
            (state.playing, ow_ipc::TRANSPORT_PLAYING),
            (state.counting_in, ow_ipc::TRANSPORT_COUNTING_IN),
            (state.recording, ow_ipc::TRANSPORT_RECORDING),
        ] {
            if on {
                flags |= bit;
            }
        }
        self.telemetry
            .transport_flags
            .store(flags, Ordering::Relaxed);
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

    /// Installs a queued recording (an earlier one that ended is retired first).
    fn swap_pending_record(&mut self) {
        if self.pending_retire_record.is_some() {
            return;
        }
        if let Some(new) = self.records.pop() {
            if let Some(mut old) = self.record.replace(new) {
                old.end();
                self.pending_retire_record = Some(old);
            }
            self.flush_record();
        }
    }

    /// Hands an ended recording back to the control thread; keeps it for the next cycle when the
    /// retire ring is full, so the audio thread never frees it.
    fn flush_record(&mut self) {
        if self.pending_retire_record.is_none()
            && self.record.as_deref().is_some_and(RecordSet::is_ended)
        {
            self.pending_retire_record = self.record.take();
        }
        if let Some(set) = self.pending_retire_record.take()
            && let Err(set) = self.retire_records.try_push(set)
        {
            self.pending_retire_record = Some(set);
        }
    }

    /// Installs a queued preview; one that is playing is ended and retired.
    fn swap_pending_preview(&mut self) {
        if self.pending_retire_preview.is_some() {
            return;
        }
        if let Some(new) = self.previews.pop() {
            // The preview that was playing keeps playing while it fades out (no hard cut); one
            // that was still fading from an earlier replacement is ended and retired.
            if let Some(mut old) = self.preview.replace(new) {
                old.fade_out();
                if let Some(mut fading) = self.fading_preview.replace(old) {
                    fading.end();
                    self.pending_retire_preview = Some(fading);
                }
            }
            self.flush_preview();
        }
    }

    /// Hands an ended preview back to the control thread; keeps it for the next cycle when the
    /// retire ring is full, so the audio thread never frees it.
    fn flush_preview(&mut self) {
        if self.pending_retire_preview.is_none()
            && self.preview.as_deref().is_some_and(Preview::is_ended)
        {
            self.pending_retire_preview = self.preview.take();
        }
        if self.pending_retire_preview.is_none()
            && self
                .fading_preview
                .as_deref()
                .is_some_and(Preview::is_ended)
        {
            self.pending_retire_preview = self.fading_preview.take();
        }
        if let Some(preview) = self.pending_retire_preview.take()
            && let Err(preview) = self.retire_previews.try_push(preview)
        {
            self.pending_retire_preview = Some(preview);
        }
    }

    fn drain_rings(&mut self) {
        let Self {
            rings,
            current,
            transport,
            telemetry,
            timeline,
            record,
            midi_pending,
            midi_seq,
            stream_frames,
            cycle_us,
            ..
        } = self;
        let rate = current
            .as_deref()
            .map_or(NO_PLAN_RATE, |plan| plan.data().sample_rate);
        for ring in rings.iter_mut() {
            while let Some(message) = ring.pop() {
                let routed = match message {
                    // SAFETY (both arms): audio thread, between cycles.
                    Message::SetParam { node, param, value } => current
                        .as_deref()
                        .is_some_and(|plan| unsafe { plan.data().set_param(node, param, value) }),
                    Message::MidiIn {
                        message,
                        timestamp_us,
                    } => {
                        let behind_us = *cycle_us as i128 - i128::from(timestamp_us);
                        let delta = (-(behind_us as f64) * f64::from(rate) / 1e6).round() as i64;
                        let window = (
                            -MIDI_WINDOW_PAST * i64::from(rate),
                            MIDI_WINDOW_FUTURE_MS * i64::from(rate) / 1000,
                        );
                        let frame = *stream_frames as i64 + delta.clamp(window.0, window.1);
                        if midi_pending.len() < midi_pending.capacity() {
                            midi_pending.push(PendingMidi {
                                frame,
                                seq: *midi_seq,
                                message,
                            });
                            *midi_seq += 1;
                            true
                        } else {
                            false
                        }
                    }
                    Message::Midi { node, message, .. } => current
                        .as_deref()
                        .is_some_and(|plan| unsafe { plan.data().midi(node, message) }),
                    Message::Transport(command) => {
                        transport.apply(command, timeline);
                        true
                    }
                    Message::EndRecording => {
                        if let Some(set) = record.as_deref_mut() {
                            set.end();
                        }
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
