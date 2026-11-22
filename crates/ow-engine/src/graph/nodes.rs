//! Built-in nodes used by the engine itself and by tests. Instruments and effects live in
//! later stages.

use std::f64::consts::TAU;

use super::{Node, ProcessContext};
use crate::{TEST_TONE_HZ, ToneGenerator};

/// Parameter index: oscillator frequency in hertz.
pub const OSC_PARAM_FREQ: u32 = 0;
/// Parameter index: oscillator amplitude (linear).
pub const OSC_PARAM_AMP: u32 = 1;
/// Parameter index: gain (linear) for [`GainNode`] and [`MasterNode`].
pub const GAIN_PARAM: u32 = 0;
/// Parameter index: test tone on (`>= 0.5`) or off.
pub const TONE_PARAM_ON: u32 = 0;

/// Mono sine oscillator with a double-precision phase accumulator.
#[derive(Debug, Clone)]
pub struct OscillatorNode {
    freq: f64,
    amp: f32,
    phase: f64,
}

impl OscillatorNode {
    /// A sine at `freq` hertz with peak `amp`, starting at phase 0.
    pub fn new(freq: f32, amp: f32) -> Self {
        Self {
            freq: f64::from(freq),
            amp,
            phase: 0.0,
        }
    }
}

impl Node for OscillatorNode {
    fn name(&self) -> &'static str {
        "oscillator"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let step = self.freq / f64::from(ctx.sample_rate.max(1));
        let amp = f64::from(self.amp);
        let mut phase = self.phase;
        for sample in ctx.outputs.channel_mut(0) {
            *sample = ((phase * TAU).sin() * amp) as f32;
            phase += step;
            // Robust against large steps (high frequency, low sample rate).
            phase -= phase.floor();
        }
        self.phase = phase;
    }
    fn set_param(&mut self, param: u32, value: f32) {
        // Non-finite values (and negative frequencies) are ignored, so a bad message cannot
        // poison the node's state.
        if !value.is_finite() {
            return;
        }
        match param {
            OSC_PARAM_FREQ if value >= 0.0 => self.freq = f64::from(value),
            OSC_PARAM_AMP => self.amp = value,
            _ => {}
        }
    }
}

/// Multiplies every channel by a gain that ramps linearly across the block after a change.
#[derive(Debug, Clone)]
pub struct GainNode {
    channels: usize,
    current: f32,
    target: f32,
}

impl GainNode {
    /// A gain over `channels` channels starting at `gain` (linear).
    pub fn new(channels: usize, gain: f32) -> Self {
        Self {
            channels,
            current: gain,
            target: gain,
        }
    }
}

impl Node for GainNode {
    fn name(&self) -> &'static str {
        "gain"
    }
    fn input_channels(&self) -> usize {
        self.channels
    }
    fn output_channels(&self) -> usize {
        self.channels
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let step = (self.target - self.current) / ctx.frames as f32;
        for channel in 0..self.channels {
            let input = ctx.inputs.channel(channel);
            let output = ctx.outputs.channel_mut(channel);
            if step == 0.0 {
                let gain = self.target;
                for (out, sample) in output.iter_mut().zip(input) {
                    *out = sample * gain;
                }
            } else {
                let mut gain = self.current;
                for (out, sample) in output.iter_mut().zip(input) {
                    gain += step;
                    *out = sample * gain;
                }
            }
        }
        self.current = self.target;
    }
    fn set_param(&mut self, param: u32, value: f32) {
        if param == GAIN_PARAM && value.is_finite() {
            self.target = value;
        }
    }
}

/// Stereo master: a gain stage whose two outputs are what the device plays.
#[derive(Debug, Clone)]
pub struct MasterNode {
    gain: GainNode,
}

impl MasterNode {
    /// A stereo master at unity gain.
    pub fn new() -> Self {
        Self {
            gain: GainNode::new(2, 1.0),
        }
    }
}

impl Default for MasterNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for MasterNode {
    fn name(&self) -> &'static str {
        "master"
    }
    fn input_channels(&self) -> usize {
        2
    }
    fn output_channels(&self) -> usize {
        2
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        self.gain.process(ctx);
    }
    fn set_param(&mut self, param: u32, value: f32) {
        self.gain.set_param(param, value);
    }
}

/// Mono sum of N inputs. The graph compiler inserts one where several edges meet at one input.
#[derive(Debug, Clone)]
pub struct SumNode {
    inputs: usize,
}

impl SumNode {
    /// A sum of `inputs` channels into one output channel.
    pub fn new(inputs: usize) -> Self {
        Self { inputs }
    }
}

impl Node for SumNode {
    fn name(&self) -> &'static str {
        "sum"
    }
    fn input_channels(&self) -> usize {
        self.inputs
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let out = ctx.outputs.channel_mut(0);
        out.fill(0.0);
        for channel in 0..self.inputs {
            for (sum, sample) in out.iter_mut().zip(ctx.inputs.channel(channel)) {
                *sum += sample;
            }
        }
    }
}

/// The stage 1 smoke tone (440 Hz, faded) as a mono source; parameter [`TONE_PARAM_ON`]
/// switches it.
#[derive(Debug, Clone)]
pub struct TestToneNode {
    tone: ToneGenerator,
    active: bool,
}

impl TestToneNode {
    /// A silent tone generator; switch it on with [`TONE_PARAM_ON`].
    pub fn new() -> Self {
        Self {
            tone: ToneGenerator::new(48_000, TEST_TONE_HZ),
            active: false,
        }
    }
}

impl Default for TestToneNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for TestToneNode {
    fn name(&self) -> &'static str {
        "test-tone"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn prepare(&mut self, sample_rate: u32, _max_block: usize) {
        self.tone = ToneGenerator::new(sample_rate, TEST_TONE_HZ);
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let out = ctx.outputs.channel_mut(0);
        out.fill(0.0);
        self.tone.render(out, 1, self.active);
    }
    fn set_param(&mut self, param: u32, value: f32) {
        if param == TONE_PARAM_ON {
            self.active = value >= 0.5;
        }
    }
}
