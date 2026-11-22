//! The audio-thread side of the engine ([`AudioCore`]) and the control-thread side
//! ([`ControlLink`]), joined by wait-free rings (architecture section 4).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use assert_no_alloc::assert_no_alloc;

use super::NodeId;
use crate::plan::RenderPlan;
use crate::rings::{ProducerRing, RingSender, producer_ring};

/// Transport state handed to nodes each block. Tempo and timeline come in stage 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Transport {
    /// Whether the transport is running.
    pub playing: bool,
    /// Position in samples at the start of the block.
    pub position: u64,
}

/// A transport change sent through a message ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportCommand {
    /// Start advancing the position.
    Play,
    /// Stop advancing the position.
    Stop,
    /// Jump to a sample position.
    Locate(u64),
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

/// Counters the audio thread publishes; read from any thread.
#[derive(Debug, Default)]
pub struct Telemetry {
    cycles: AtomicU64,
    plan_swaps: AtomicU64,
    unrouted: AtomicU64,
}

impl Telemetry {
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

/// Assembles an [`AudioCore`] and its [`ControlLink`], registering extra producer rings.
pub struct CoreBuilder {
    plan_queue: usize,
    retire_queue: Option<usize>,
    control_ring: usize,
    rings: Vec<ProducerRing<Message>>,
}

impl CoreBuilder {
    /// A builder with a plan queue of 8 plans and a control ring of 1024 messages.
    pub fn new() -> Self {
        Self {
            plan_queue: 8,
            retire_queue: None,
            control_ring: 1024,
            rings: Vec::new(),
        }
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

    /// Creates both halves. The control thread keeps the [`ControlLink`]; the audio callback
    /// owns the [`AudioCore`].
    pub fn build(self) -> (ControlLink, AudioCore) {
        let (plan_tx, plan_rx) = producer_ring::<Box<RenderPlan>>(self.plan_queue);
        // One more slot than plans in flight, so the audio thread can always retire.
        let (retire_tx, retire_rx) =
            producer_ring::<Box<RenderPlan>>(self.retire_queue.unwrap_or(self.plan_queue + 2));
        let (control_tx, control_rx) = producer_ring::<Message>(self.control_ring);
        let telemetry = Arc::new(Telemetry::default());
        let mut rings = Vec::with_capacity(self.rings.len() + 1);
        rings.push(control_rx);
        rings.extend(self.rings);
        (
            ControlLink {
                plans: plan_tx,
                retired: retire_rx,
                messages: control_tx,
                telemetry: Arc::clone(&telemetry),
            },
            AudioCore {
                plans: plan_rx,
                retire: retire_tx,
                rings,
                current: None,
                pending_retire: None,
                transport: Transport::default(),
                telemetry,
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
    telemetry: Arc<Telemetry>,
}

impl ControlLink {
    /// Queues a plan for the audio thread to swap in at its next cycle boundary. Also frees any
    /// plans the audio thread has already retired. Returns the plan if the ring is full.
    pub fn swap_plan(&mut self, plan: RenderPlan) -> Result<(), SwapError> {
        self.collect_retired();
        self.plans.try_push(Box::new(plan)).map_err(SwapError)
    }

    /// Drops plans the audio thread has retired (this is where their memory is freed) and
    /// returns how many.
    pub fn collect_retired(&mut self) -> usize {
        let mut dropped = 0;
        while self.retired.pop().is_some() {
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

/// Audio-thread end: owns the current plan and runs one engine cycle per device buffer.
///
/// [`process`](Self::process) is allocation-free, lock-free and wait-free, and runs under
/// `assert_no_alloc` (active in debug and test builds when the binary installs
/// `assert_no_alloc::AllocDisabler` as its global allocator).
pub struct AudioCore {
    plans: ProducerRing<Box<RenderPlan>>,
    retire: RingSender<Box<RenderPlan>>,
    rings: Vec<ProducerRing<Message>>,
    current: Option<Box<RenderPlan>>,
    pending_retire: Option<Box<RenderPlan>>,
    transport: Transport,
    telemetry: Arc<Telemetry>,
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
        self.transport
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
        assert_no_alloc(|| self.cycle(out, channels));
    }

    fn cycle(&mut self, out: &mut [f32], channels: usize) {
        self.telemetry.cycles.fetch_add(1, Ordering::Relaxed);
        self.flush_retired();
        self.swap_pending_plan();
        self.drain_rings();
        out.fill(0.0);
        let channels = channels.max(1);
        let total = out.len() / channels;
        let mut done = 0;
        while done < total {
            let Some(plan) = self.current.as_deref_mut() else {
                // No plan: silence, but keep the transport moving.
                self.advance(total - done);
                break;
            };
            let frames = (total - done).min(plan.max_block());
            plan.process_block(frames, self.transport);
            for channel in 0..channels {
                let source = if plan.master_channels() == 1 {
                    plan.master_channel(0, frames)
                } else {
                    plan.master_channel(channel, frames)
                };
                if let Some(source) = source {
                    for (frame, sample) in source.iter().enumerate() {
                        out[(done + frame) * channels + channel] =
                            if sample.is_finite() { *sample } else { 0.0 };
                    }
                }
            }
            done += frames;
            self.advance(frames);
        }
    }

    fn advance(&mut self, frames: usize) {
        if self.transport.playing {
            self.transport.position += frames as u64;
        }
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
            self.pending_retire = self.current.replace(new);
            self.telemetry.plan_swaps.fetch_add(1, Ordering::Relaxed);
            self.flush_retired();
        }
    }

    fn drain_rings(&mut self) {
        let Self {
            rings,
            current,
            transport,
            telemetry,
            ..
        } = self;
        for ring in rings.iter_mut() {
            while let Some(message) = ring.pop() {
                let routed = match message {
                    Message::SetParam { node, param, value } => current
                        .as_deref_mut()
                        .is_some_and(|plan| plan.set_param(node, param, value)),
                    Message::Midi { node, message, .. } => current
                        .as_deref_mut()
                        .is_some_and(|plan| plan.midi(node, message)),
                    Message::Transport(command) => {
                        match command {
                            TransportCommand::Play => transport.playing = true,
                            TransportCommand::Stop => transport.playing = false,
                            TransportCommand::Locate(position) => transport.position = position,
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
