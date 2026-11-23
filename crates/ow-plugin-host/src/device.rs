//! [`PluginDevice`]: a hosted plugin as an `ow-dsp` [`Device`], which is what the engine's
//! device node runs.
//!
//! The engine already turns an `ow-dsp` device into a stereo graph node (`DeviceNode`), so
//! hosting a plugin is only a matter of presenting it as a device. Parameters do not travel
//! through [`Device::set_param`]: the node's parameter values are fixed when it is built, and
//! live edits go through [`HostedPlugin::set_params`](crate::HostedPlugin::set_params)'s own
//! lock-free queue, which is owned by the plugin instance and so survives recompiles.

use ow_dsp::{Device, Event};

use crate::hosted::PluginProcessor;

/// A hosted plugin's processor presented as a device.
pub struct PluginDevice {
    processor: Box<dyn PluginProcessor>,
}

impl PluginDevice {
    /// A device over `processor`.
    pub fn new(processor: Box<dyn PluginProcessor>) -> Self {
        Self { processor }
    }
}

impl Device for PluginDevice {
    /// The instance was configured when it was acquired; nothing to do here.
    fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}

    /// Plugin parameters are edited through the instance's queue, not through the node.
    fn set_param(&mut self, _id: u32, _value: f64) {}

    fn reset(&mut self) {
        self.processor.reset();
    }

    fn process(&mut self, audio: &mut [&mut [f32]; 2], events: &[Event]) {
        self.processor.process(audio, events);
    }

    fn latency_frames(&self) -> u32 {
        self.processor.latency_frames()
    }
}
