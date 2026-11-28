//! [`DeviceNode`]: an `ow-dsp` [`Device`] as a stereo graph node.
//!
//! Instruments and effects share this one node. An *instrument* node has no audio inputs; its
//! events come from the track's [`MidiClipPlayer`], which runs inside the node because graph
//! edges carry audio only. An *effect* node has two inputs (left and right of the signal before
//! it in the chain) and no player. Either way the node has two outputs, and the device processes
//! stereo in place: the node copies the inputs (or silence) into two preallocated buffers, runs
//! the device and copies the result out, so a device never sees the graph's buffer types. Non-finite
//! input samples are replaced with silence on the way in, so one bad sample cannot poison an
//! effect's filter or feedback memory.
//!
//! Live MIDI input. An instrument built with [`DeviceNode::with_live_input`] also plays the
//! notes a person plays: the plan hands it each input message ([`Node::on_live_midi`]) with the
//! frame of the coming block it belongs to, and it merges them with the clip events in frame
//! order. They sound while its [`LiveGate`] is open (the track is armed, or monitoring is on) and
//! a note that is still held when the gate closes is released. Everything is preallocated, so
//! this adds no allocation or lock to the audio thread.
//!
//! Parameters are fixed when the node is built from the model's device; `Node::set_param`
//! forwards a live value (clamped to the descriptor's range) for callers that want to change one
//! without a recompile.
//!
//! A hosted third-party plugin is a device too (`ow_plugin_host::PluginDevice`), so it runs in
//! the same node, built with [`DeviceNode::hosted_instrument`] or [`DeviceNode::hosted_effect`].
//! It has no static descriptor, and its live parameter edits do not pass through the node: they
//! go through the plugin instance's own queue, which survives the recompile that replaces this
//! node.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ow_dsp::{Device, DeviceInfo, Event, EventKind};

use crate::clips::midi::MidiClipPlayer;
use crate::graph::{MidiMessage, Node, ProcessContext};

/// Most note events one block can carry into a device; more are dropped.
pub const MAX_EVENTS_PER_BLOCK: usize = 4096;

/// Most live events one block holds for a device.
///
/// Note-ons and controller changes stop at [`LIVE_SOFT_LIMIT`]; the rest is reserved for
/// note-offs, which are never dropped: a block holds at most `LIVE_SOFT_LIMIT` note-ons, so at
/// most that many offs plus the 128 keys held from earlier blocks, 128 deferred offs due now,
/// and 128 offs of a gate closing, all below this size.
const MAX_LIVE_EVENTS: usize = 1024;

/// Live note-ons and controller changes are accepted while fewer than this many events wait.
const LIVE_SOFT_LIMIT: usize = 512;

/// When an instrument plays live MIDI input.
#[derive(Debug, Clone)]
pub enum LiveGate {
    /// Always.
    Always,
    /// While the flag is set (the track is armed for recording).
    Armed(Arc<AtomicBool>),
}

impl LiveGate {
    fn is_open(&self) -> bool {
        match self {
            Self::Always => true,
            Self::Armed(flag) => flag.load(Ordering::Relaxed),
        }
    }
}

/// A built-in device running as a node.
pub struct DeviceNode {
    /// The built-in device's descriptor; `None` for a hosted plugin.
    info: Option<&'static DeviceInfo>,
    device: Box<dyn Device>,
    player: Option<MidiClipPlayer>,
    inputs: usize,
    left: Vec<f32>,
    right: Vec<f32>,
    events: Vec<Event>,
    /// Keyed copies of `events` for the allocation-free sort.
    sort_scratch: Vec<(u64, Event)>,
    live: Option<LiveGate>,
    live_events: Vec<Event>,
    /// Keys the live input holds down.
    live_held: [bool; 128],
    live_held_count: u32,
    /// The live input has sent a sustain pedal down that no lift has followed.
    live_pedal: bool,
    /// Keys whose live note-off arrived on the frame of their own note-on: the off is sent at
    /// frame 0 of the next block, so the note sounds for a moment instead of sticking.
    live_deferred: [bool; 128],
    /// Deferred offs that were set in an earlier block and go out in this one.
    live_due: [bool; 128],
}

impl DeviceNode {
    /// An instrument node: no audio inputs, notes from `player`.
    pub fn instrument(
        info: &'static DeviceInfo,
        device: Box<dyn Device>,
        player: MidiClipPlayer,
    ) -> Self {
        Self::build(Some(info), device, Some(player), 0)
    }

    /// An effect node: stereo in, stereo out, no events.
    pub fn effect(info: &'static DeviceInfo, device: Box<dyn Device>) -> Self {
        Self::build(Some(info), device, None, 2)
    }

    /// An instrument node over a hosted plugin: no audio inputs, notes from `player`.
    pub fn hosted_instrument(device: Box<dyn Device>, player: MidiClipPlayer) -> Self {
        Self::build(None, device, Some(player), 0)
    }

    /// An effect node over a hosted plugin: stereo in, stereo out.
    pub fn hosted_effect(device: Box<dyn Device>) -> Self {
        Self::build(None, device, None, 2)
    }

    fn build(
        info: Option<&'static DeviceInfo>,
        device: Box<dyn Device>,
        player: Option<MidiClipPlayer>,
        inputs: usize,
    ) -> Self {
        Self {
            info,
            device,
            player,
            inputs,
            left: Vec::new(),
            right: Vec::new(),
            events: Vec::new(),
            sort_scratch: Vec::new(),
            live: None,
            live_events: Vec::new(),
            live_held: [false; 128],
            live_held_count: 0,
            live_pedal: false,
            live_deferred: [false; 128],
            live_due: [false; 128],
        }
    }

    /// Makes an instrument node play live MIDI input while `gate` is open.
    pub fn with_live_input(mut self, gate: LiveGate) -> Self {
        self.live = Some(gate);
        self
    }

    /// Releases every key the live input holds, and lifts a forwarded sustain pedal, at frame 0.
    fn release_live(&mut self) {
        if self.live_pedal {
            self.live_pedal = false;
            if self.live_events.len() < MAX_LIVE_EVENTS {
                self.live_events.push(Event::control_change(0, 64, 0));
            }
        }
        for key in 0..128_u8 {
            if self.live_held[usize::from(key)] {
                self.live_held[usize::from(key)] = false;
                if self.live_events.len() < MAX_LIVE_EVENTS {
                    self.live_events.push(Event::note_off(0, key));
                }
            }
        }
        self.live_held_count = 0;
    }
}

/// Orders events by frame, in place and without allocating: at one frame control changes come
/// first and keep their arrival order (so a pedal down then up at one frame ends up), then
/// note-offs, then note-ons (so a key retriggers); notes of one kind are fixed by key and
/// velocity, so their order never depends on arrival.
///
/// `scratch` holds a keyed copy of the events; the node's is as large as its event buffer. Should
/// it be smaller than `events` (it never is for the node) the surplus is trimmed first, as
/// [`trim_events`] does, rather than panicking. The sort is `O(n log n)`.
pub fn order_events(events: &mut Vec<Event>, scratch: &mut Vec<(u64, Event)>) {
    let excess = events.len().saturating_sub(scratch.capacity());
    trim_events(events, excess);
    scratch.clear();
    for (index, event) in events.iter().enumerate() {
        // Key bits: offset (32), class (2: control, off, on), then arrival or key and velocity.
        let (class, low) = match event.kind {
            EventKind::ControlChange { .. } => (0_u64, index as u64 & 0x3FFF_FFFF),
            EventKind::NoteOff { key } => (1, u64::from(key) << 8),
            EventKind::NoteOn { key, velocity } => (2, u64::from(key) << 8 | u64::from(velocity)),
        };
        scratch.push((u64::from(event.offset) << 32 | class << 30 | low, *event));
    }
    scratch.sort_unstable_by_key(|(key, _)| *key);
    for (slot, (_, event)) in events.iter_mut().zip(scratch.iter()) {
        *slot = *event;
    }
}

/// Removes `excess` events from the tail, allocation-free: first note-ons and controller
/// changes (latest first), then, only if that is not enough, note-offs, so a trimmed block never
/// leaves a sounding note without its release.
pub fn trim_events(events: &mut Vec<Event>, excess: usize) {
    let mut remaining = excess.min(events.len());
    for pass in 0..2 {
        if remaining == 0 {
            break;
        }
        let len = events.len();
        let mut write = len;
        for read in (0..len).rev() {
            let event = events[read];
            let droppable = pass == 1 || !matches!(event.kind, EventKind::NoteOff { .. });
            if remaining > 0 && droppable {
                remaining -= 1;
            } else {
                write -= 1;
                events[write] = event;
            }
        }
        events.drain(..write);
    }
}

impl Node for DeviceNode {
    fn name(&self) -> &'static str {
        "device"
    }

    fn input_channels(&self) -> usize {
        self.inputs
    }

    fn output_channels(&self) -> usize {
        2
    }

    /// The device's own latency, so delay compensation lines its path up with the others. It is
    /// read when a graph is compiled, from the device built with the model's parameters.
    fn latency_samples(&self) -> u32 {
        self.device.latency_frames()
    }

    fn prepare(&mut self, sample_rate: u32, max_block: usize) {
        self.left = vec![0.0; max_block];
        self.right = vec![0.0; max_block];
        self.events = Vec::with_capacity(MAX_EVENTS_PER_BLOCK);
        self.sort_scratch = Vec::with_capacity(self.events.capacity());
        self.live_events = Vec::with_capacity(MAX_LIVE_EVENTS);
        self.device.prepare(sample_rate as f32, max_block);
    }

    fn accepts_live_midi(&self) -> bool {
        self.live.is_some()
    }

    fn on_live_midi(&mut self, offset: u32, message: MidiMessage) {
        let Some(gate) = &self.live else {
            return;
        };
        let open = gate.is_open();
        let key = message.data1 & 0x7F;
        match message.status & 0xF0 {
            0x90 if message.data2 > 0 => {
                if open && self.live_events.len() < LIVE_SOFT_LIMIT {
                    if !self.live_held[usize::from(key)] {
                        self.live_held[usize::from(key)] = true;
                        self.live_held_count += 1;
                    }
                    self.live_events
                        .push(Event::note_on(offset, key, message.data2));
                }
            }
            // A note-off always passes for a key the input holds, gate or not.
            0x80 | 0x90 if self.live_held[usize::from(key)] => {
                self.live_held[usize::from(key)] = false;
                self.live_held_count -= 1;
                let same_frame = self.live_events.iter().any(|e| {
                    e.offset >= offset
                        && matches!(e.kind, EventKind::NoteOn { key: k, .. } if k == key)
                });
                if same_frame {
                    self.live_deferred[usize::from(key)] = true;
                } else if self.live_events.len() < MAX_LIVE_EVENTS {
                    self.live_events.push(Event::note_off(offset, key));
                }
            }
            // A control change (mod wheel, pedal) plays while the gate is open; a pedal lift
            // always passes so a pedal held when the gate closed cannot stick.
            0xB0 => {
                let value = message.data2 & 0x7F;
                // A lift of a pedal this input holds down is a state change that must get through
                // (even with the gate closed or the soft limit reached); any other CC is not.
                let lift = key == 64 && value < 64 && self.live_pedal;
                let room = if lift {
                    MAX_LIVE_EVENTS
                } else {
                    LIVE_SOFT_LIMIT
                };
                if (open || lift) && self.live_events.len() < room {
                    if key == 64 {
                        self.live_pedal = value >= 64;
                    }
                    self.live_events
                        .push(Event::control_change(offset, key, value));
                }
            }
            _ => {}
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.frames;
        self.events.clear();
        if let Some(player) = &mut self.player {
            player.generate(&ctx.timing, &mut self.events);
        }
        // Ordering contract: offs that fell due from an earlier block are pushed here, before
        // the merge and sort below, so at frame 0 they come before any note-on of the same key.
        for key in 0..128_u8 {
            if self.live_due[usize::from(key)] && self.live_events.len() < MAX_LIVE_EVENTS {
                self.live_events.push(Event::note_off(0, key));
            }
        }
        // What was deferred while this block was being prepared is due in the next one.
        self.live_due = self.live_deferred;
        self.live_deferred = [false; 128];
        // The gate is read once per block. If it closed during the cycle, note-ons accepted
        // while it was open are moot: drop them, so a release at frame 0 cannot sort before
        // one and leave its note hanging.
        let gate_closed = self.live.as_ref().is_some_and(|gate| !gate.is_open());
        if gate_closed {
            self.live_events
                .retain(|event| !matches!(event.kind, EventKind::NoteOn { .. }));
            if self.live_held_count > 0 || self.live_pedal {
                self.release_live();
            }
        }
        if !self.live_events.is_empty() {
            // Live events always fit: if the clip events leave too little room, the tail of the
            // block's clip events gives way, note-ons and controllers before note-offs.
            let needed = self.events.len() + self.live_events.len();
            let excess = needed.saturating_sub(self.events.capacity());
            trim_events(&mut self.events, excess);
            for event in self.live_events.drain(..) {
                if self.events.len() < self.events.capacity() {
                    self.events.push(event);
                }
            }
            order_events(&mut self.events, &mut self.sort_scratch);
        }
        if self.inputs == 2 {
            // A NaN or infinity would stay in an effect's feedback state for good, so a
            // non-finite input sample reaches the device as silence.
            let finite = |s: &f32| if s.is_finite() { *s } else { 0.0 };
            for (dst, src) in self.left[..frames].iter_mut().zip(ctx.inputs.channel(0)) {
                *dst = finite(src);
            }
            for (dst, src) in self.right[..frames].iter_mut().zip(ctx.inputs.channel(1)) {
                *dst = finite(src);
            }
        } else {
            self.left[..frames].fill(0.0);
            self.right[..frames].fill(0.0);
        }
        self.device.process(
            &mut [&mut self.left[..frames], &mut self.right[..frames]],
            &self.events,
        );
        ctx.outputs
            .channel_mut(0)
            .copy_from_slice(&self.left[..frames]);
        ctx.outputs
            .channel_mut(1)
            .copy_from_slice(&self.right[..frames]);
    }

    fn set_param(&mut self, param: u32, value: f32) {
        if let Some(descriptor) = self.info.and_then(|info| info.param(param)) {
            self.device
                .set_param(param, descriptor.clamp(f64::from(value)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trimming_drops_note_ons_and_controllers_before_note_offs() {
        let mut events = vec![
            Event::note_on(1, 60, 100),
            Event::note_off(2, 60),
            Event::control_change(3, 1, 5),
            Event::note_on(4, 62, 100),
            Event::note_off(5, 62),
        ];
        // Two to drop: the latest note-on and the controller before it go; both offs stay.
        trim_events(&mut events, 2);
        assert_eq!(
            events,
            [
                Event::note_on(1, 60, 100),
                Event::note_off(2, 60),
                Event::note_off(5, 62),
            ]
        );
        // More than there are note-ons and controllers: offs go, latest first.
        trim_events(&mut events, 2);
        assert_eq!(events, [Event::note_off(2, 60)]);
        trim_events(&mut events, 10);
        assert!(events.is_empty());
    }

    #[test]
    fn an_undersized_scratch_trims_instead_of_panicking() {
        let mut events = vec![
            Event::note_on(1, 60, 100),
            Event::note_off(2, 60),
            Event::note_on(3, 61, 100),
        ];
        let mut scratch = Vec::with_capacity(2);
        let cap = scratch.capacity();
        order_events(&mut events, &mut scratch);
        assert_eq!(events.len(), 3.min(cap));
        assert!(events.contains(&Event::note_off(2, 60)));
    }

    fn cc(offset: u32, controller: u8, value: u8) -> Event {
        Event::control_change(offset, controller, value)
    }

    #[test]
    fn same_frame_control_changes_keep_their_arrival_order() {
        // Pedal down then up at one frame stays down-then-up, however the values compare.
        let mut events = vec![cc(10, 64, 127), cc(10, 64, 0)];
        order_events(&mut events, &mut Vec::with_capacity(16));
        assert_eq!(events, [cc(10, 64, 127), cc(10, 64, 0)]);
        let mut events = vec![cc(10, 64, 0), cc(10, 64, 127)];
        order_events(&mut events, &mut Vec::with_capacity(16));
        assert_eq!(events, [cc(10, 64, 0), cc(10, 64, 127)]);
        // Repeated CC1 at one frame: the last arrival stays last.
        let mut events = vec![cc(5, 1, 100), cc(5, 1, 20), cc(5, 1, 60)];
        order_events(&mut events, &mut Vec::with_capacity(16));
        assert_eq!(events, [cc(5, 1, 100), cc(5, 1, 20), cc(5, 1, 60)]);
    }

    #[test]
    fn a_frame_orders_control_changes_then_note_offs_then_note_ons() {
        let mut events = vec![
            Event::note_on(7, 60, 100),
            Event::note_off(7, 60),
            cc(7, 64, 127),
            Event::note_on(3, 61, 90),
            Event::note_on(7, 59, 80),
        ];
        order_events(&mut events, &mut Vec::with_capacity(16));
        assert_eq!(
            events,
            [
                Event::note_on(3, 61, 90),
                cc(7, 64, 127),
                Event::note_off(7, 60),
                Event::note_on(7, 59, 80),
                Event::note_on(7, 60, 100),
            ]
        );
    }
}
