//! Offline render driver: runs a plan through the same [`AudioCore`] cycle the device callback
//! uses, as fast as the CPU allows, for tests and (later) export.

use crate::graph::{AudioCore, ControlLink, CoreBuilder, Message, Transport};
use crate::plan::RenderPlan;

/// Drives an [`AudioCore`] synchronously from the calling thread.
pub struct OfflineRenderer {
    link: ControlLink,
    core: AudioCore,
    channels: usize,
}

impl OfflineRenderer {
    /// A renderer producing `channels` interleaved output channels from `plan`, running every
    /// node on the calling thread.
    pub fn new(plan: RenderPlan, channels: usize) -> Self {
        Self::with_workers(plan, channels, 0)
    }

    /// Like [`new`](Self::new) with `workers` DSP worker threads (normal priority) helping the
    /// calling thread. The output is bit-identical to [`new`](Self::new).
    pub fn with_workers(plan: RenderPlan, channels: usize, workers: usize) -> Self {
        let (mut link, core) = CoreBuilder::new().workers(workers).build();
        assert!(
            link.swap_plan(plan).is_ok(),
            "a fresh plan queue has room for one plan"
        );
        Self {
            link,
            core,
            channels: channels.max(1),
        }
    }

    /// Control link, to send parameter and transport messages or further plans between blocks.
    pub fn link(&mut self) -> &mut ControlLink {
        &mut self.link
    }

    /// Transport state after the last rendered block.
    pub fn transport(&self) -> Transport {
        self.core.transport()
    }

    /// Sends a message to the audio core (applied at the start of the next block).
    pub fn send(&mut self, message: Message) -> bool {
        self.link.send(message)
    }

    /// Renders `frames` frames in device-sized blocks of `block` frames and returns them
    /// interleaved (`frames * channels` samples).
    pub fn render(&mut self, frames: usize, block: usize) -> Vec<f32> {
        self.render_with_input(frames, block, None)
    }

    /// Like [`render`](Self::render) with interleaved device input audio of `frames * channels`
    /// samples, handed to the core block by block (currently ignored by the core).
    pub fn render_with_input(
        &mut self,
        frames: usize,
        block: usize,
        input: Option<&[f32]>,
    ) -> Vec<f32> {
        let block = block.max(1);
        let mut out = vec![0.0; frames * self.channels];
        for (index, chunk) in out.chunks_mut(block * self.channels).enumerate() {
            let start = index * block * self.channels;
            let chunk_input = input.map(|input| &input[start..start + chunk.len()]);
            self.core.process(chunk, self.channels, chunk_input);
        }
        out
    }
}
