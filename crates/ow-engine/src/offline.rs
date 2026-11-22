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
    /// A renderer producing `channels` interleaved output channels from `plan`.
    pub fn new(plan: RenderPlan, channels: usize) -> Self {
        let (mut link, core) = CoreBuilder::new().build();
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
        let block = block.max(1);
        let mut out = vec![0.0; frames * self.channels];
        for chunk in out.chunks_mut(block * self.channels) {
            self.core.process(chunk, self.channels);
        }
        out
    }
}
