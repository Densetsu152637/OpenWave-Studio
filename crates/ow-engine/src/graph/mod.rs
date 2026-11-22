//! The audio graph: nodes, the per-block [`ProcessContext`] they run in, and the
//! control-thread [`Graph`] that compiles into an immutable [`RenderPlan`](crate::RenderPlan).
//!
//! Buffers are non-interleaved `f32`, one slice per channel, at most [`MAX_BLOCK`] frames.

mod builder;
pub mod nodes;
mod runtime;

pub use builder::{Graph, GraphError};
pub use runtime::{
    AudioCore, ControlLink, CoreBuilder, Message, MidiMessage, SwapError, Telemetry, Transport,
    TransportCommand,
};

/// Largest block, in frames, a plan processes at once. Longer device buffers are split.
pub const MAX_BLOCK: usize = 4096;

/// Stable identity of a node inside a plan. Parameter and MIDI messages address nodes by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

/// A unit of audio processing.
///
/// Nodes are created and configured on the control thread, moved into a plan, and from then on
/// only touched by the audio thread. Everything on the audio side (`process`, `set_param`,
/// `on_midi`) must not allocate, lock, block or perform I/O; preallocate in `prepare` or in the
/// constructor. Channels are numbered from 0; an unconnected input reads as silence.
pub trait Node: Send {
    /// Short name for diagnostics.
    fn name(&self) -> &'static str;

    /// Number of input channels (fixed for the node's lifetime).
    fn input_channels(&self) -> usize;

    /// Number of output channels (fixed for the node's lifetime).
    fn output_channels(&self) -> usize;

    /// Processing latency in samples. Recorded for delay compensation, which is computed when a
    /// plan is compiled (stage 6); default 0.
    fn latency_samples(&self) -> u32 {
        0
    }

    /// Called once on the control thread when a plan containing the node is compiled. Allocate
    /// here, for the given `sample_rate` and largest block.
    fn prepare(&mut self, sample_rate: u32, max_block: usize) {
        let _ = (sample_rate, max_block);
    }

    /// Renders one block. The node must overwrite every output channel for `ctx.frames`
    /// samples (output buffers hold stale data from earlier blocks).
    fn process(&mut self, ctx: &mut ProcessContext<'_>);

    /// Applies a parameter value sent through a message ring, at a block boundary.
    fn set_param(&mut self, param: u32, value: f32) {
        let _ = (param, value);
    }

    /// Receives a MIDI message sent through a message ring, at a block boundary.
    fn on_midi(&mut self, message: MidiMessage) {
        let _ = message;
    }
}

/// Everything a node sees while processing one block.
///
/// `inputs` and `outputs` are separate fields so a node can read one while writing the other:
/// `let a = ctx.inputs.channel(0); let o = ctx.outputs.channel_mut(0);` borrows both at once.
pub struct ProcessContext<'a> {
    /// Frames to render in this block, `1..=MAX_BLOCK`.
    pub frames: usize,
    /// Sample rate of the plan in hertz.
    pub sample_rate: u32,
    /// Transport state at the start of the block.
    pub transport: Transport,
    /// Read-only input channels.
    pub inputs: Inputs<'a>,
    /// Writable output channels.
    pub outputs: Outputs<'a>,
}

/// Read-only view of a node's input channels for one block.
pub struct Inputs<'a> {
    pub(crate) pool: &'a [Box<[f32]>],
    pub(crate) ids: &'a [u32],
    pub(crate) frames: usize,
}

impl Inputs<'_> {
    /// Number of input channels.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// True when the node has no inputs.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Samples of input channel `channel` (`frames` long). Panics if out of range.
    pub fn channel(&self, channel: usize) -> &[f32] {
        &self.pool[self.ids[channel] as usize][..self.frames]
    }
}

/// Writable view of a node's output channels for one block.
pub struct Outputs<'a> {
    pub(crate) bufs: &'a mut [Box<[f32]>],
    pub(crate) frames: usize,
}

impl Outputs<'_> {
    /// Number of output channels.
    pub fn len(&self) -> usize {
        self.bufs.len()
    }

    /// True when the node has no outputs.
    pub fn is_empty(&self) -> bool {
        self.bufs.is_empty()
    }

    /// Mutable samples of output channel `channel` (`frames` long). Panics if out of range.
    pub fn channel_mut(&mut self, channel: usize) -> &mut [f32] {
        &mut self.bufs[channel][..self.frames]
    }
}
