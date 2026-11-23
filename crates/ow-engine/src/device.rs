//! [`DeviceNode`]: an `ow-dsp` [`Device`] as a stereo graph node.
//!
//! Instruments and effects share this one node. An *instrument* node has no audio inputs; its
//! events come from the track's [`MidiClipPlayer`], which runs inside the node because graph
//! edges carry audio only. An *effect* node has two inputs (left and right of the signal before
//! it in the chain) and no player. Either way the node has two outputs, and the device processes
//! stereo in place: the node copies the inputs (or silence) into two preallocated buffers, runs
//! the device and copies the result out, so a device never sees the graph's buffer types.
//!
//! Parameters are fixed when the node is built from the model's device; `Node::set_param`
//! forwards a live value (clamped to the descriptor's range) for callers that want to change one
//! without a recompile.

use ow_dsp::{Device, DeviceInfo, Event};

use crate::clips::midi::MidiClipPlayer;
use crate::graph::{Node, ProcessContext};

/// Most note events one block can carry into a device; more are dropped.
pub const MAX_EVENTS_PER_BLOCK: usize = 4096;

/// A built-in device running as a node.
pub struct DeviceNode {
    info: &'static DeviceInfo,
    device: Box<dyn Device>,
    player: Option<MidiClipPlayer>,
    inputs: usize,
    left: Vec<f32>,
    right: Vec<f32>,
    events: Vec<Event>,
}

impl DeviceNode {
    /// An instrument node: no audio inputs, notes from `player`.
    pub fn instrument(
        info: &'static DeviceInfo,
        device: Box<dyn Device>,
        player: MidiClipPlayer,
    ) -> Self {
        Self::build(info, device, Some(player), 0)
    }

    /// An effect node: stereo in, stereo out, no events.
    pub fn effect(info: &'static DeviceInfo, device: Box<dyn Device>) -> Self {
        Self::build(info, device, None, 2)
    }

    fn build(
        info: &'static DeviceInfo,
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
        }
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

    fn prepare(&mut self, sample_rate: u32, max_block: usize) {
        self.left = vec![0.0; max_block];
        self.right = vec![0.0; max_block];
        self.events = Vec::with_capacity(MAX_EVENTS_PER_BLOCK);
        self.device.prepare(sample_rate as f32, max_block);
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.frames;
        self.events.clear();
        if let Some(player) = &mut self.player {
            player.generate(&ctx.timing, &mut self.events);
        }
        if self.inputs == 2 {
            self.left[..frames].copy_from_slice(ctx.inputs.channel(0));
            self.right[..frames].copy_from_slice(ctx.inputs.channel(1));
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
        if let Some(descriptor) = self.info.param(param) {
            self.device
                .set_param(param, descriptor.clamp(f64::from(value)));
        }
    }
}
