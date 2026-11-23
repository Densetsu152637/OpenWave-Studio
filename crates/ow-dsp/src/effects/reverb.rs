//! `builtin.reverb`: a Freeverb-style algorithmic reverb.
//!
//! The mono sum of the input feeds eight parallel lowpass-feedback comb filters whose outputs are
//! added and run through four series allpass filters; the right channel uses the same delay
//! lengths plus a fixed stereo spread. The structure and the delay lengths (tuned for 44.1 kHz and
//! scaled to the sample rate) follow the public-domain Freeverb design; this implementation is
//! written from that description. `size` sets the comb feedback (decay time), `damping` the
//! lowpass in the feedback path (how fast high frequencies die), and `mix` crossfades between the
//! dry input and the reverb. All delay memory is allocated in `prepare`.

use crate::device::{Device, DeviceInfo, Event, ParamDescriptor, Unit};

/// The `Device::kind` of the reverb.
pub const KIND: &str = "builtin.reverb";

/// Parameter id: room size (decay length), 0 to 100 percent.
pub const PARAM_SIZE: u32 = 0;
/// Parameter id: high-frequency damping, 0 to 100 percent.
pub const PARAM_DAMPING: u32 = 1;
/// Parameter id: wet/dry mix, 0 (dry) to 100 (wet) percent.
pub const PARAM_MIX: u32 = 2;

const PARAMS: &[ParamDescriptor] = &[
    ParamDescriptor::linear(PARAM_SIZE, "Size", 0.0, 100.0, 50.0, Unit::Percent),
    ParamDescriptor::linear(PARAM_DAMPING, "Damping", 0.0, 100.0, 50.0, Unit::Percent),
    ParamDescriptor::linear(PARAM_MIX, "Mix", 0.0, 100.0, 25.0, Unit::Percent),
];

/// Static description and constructor of the reverb.
pub const INFO: DeviceInfo =
    DeviceInfo::new(KIND, "Reverb", false, PARAMS, || Box::new(Reverb::new()));

/// Comb delay lengths in samples at 44.1 kHz.
const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
/// Allpass delay lengths in samples at 44.1 kHz.
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
/// Extra delay of the right channel's filters, in samples at 44.1 kHz.
const STEREO_SPREAD: usize = 23;
/// The rate the tunings are written for.
const TUNING_RATE: f64 = 44_100.0;
/// Gain on the mono input, keeping the eight summed combs near unity.
const INPUT_GAIN: f32 = 0.015;
/// Gain on the wet signal, making full wet about as loud as the dry input.
const WET_SCALE: f32 = 3.0;
const ALLPASS_FEEDBACK: f32 = 0.5;
/// Magnitudes below this are flushed to zero so tails never become denormals.
const TINY: f32 = 1.0e-20;

fn flush(x: f32) -> f32 {
    if x.abs() < TINY { 0.0 } else { x }
}

/// A feedback comb filter with a one-pole lowpass in its loop.
#[derive(Debug, Clone)]
struct Comb {
    buffer: Vec<f32>,
    index: usize,
    filtered: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self {
            buffer: vec![0.0; len.max(1)],
            index: 0,
            filtered: 0.0,
        }
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.filtered = 0.0;
        self.index = 0;
    }

    fn tick(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let out = self.buffer[self.index];
        self.filtered = flush(out * (1.0 - damp) + self.filtered * damp);
        self.buffer[self.index] = flush(input + self.filtered * feedback);
        self.index += 1;
        if self.index == self.buffer.len() {
            self.index = 0;
        }
        out
    }
}

/// A Schroeder allpass filter.
#[derive(Debug, Clone)]
struct Allpass {
    buffer: Vec<f32>,
    index: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self {
            buffer: vec![0.0; len.max(1)],
            index: 0,
        }
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.index = 0;
    }

    fn tick(&mut self, input: f32) -> f32 {
        let delayed = self.buffer[self.index];
        let out = delayed - input;
        self.buffer[self.index] = flush(input + delayed * ALLPASS_FEEDBACK);
        self.index += 1;
        if self.index == self.buffer.len() {
            self.index = 0;
        }
        out
    }
}

/// One channel's filters.
#[derive(Debug, Clone)]
struct Channel {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
}

impl Channel {
    fn new(sample_rate: f64, spread: usize) -> Self {
        let scale =
            |len: usize| ((len + spread) as f64 * sample_rate / TUNING_RATE).round() as usize;
        Self {
            combs: COMB_TUNING.iter().map(|&n| Comb::new(scale(n))).collect(),
            allpasses: ALLPASS_TUNING
                .iter()
                .map(|&n| Allpass::new(scale(n)))
                .collect(),
        }
    }

    fn clear(&mut self) {
        self.combs.iter_mut().for_each(Comb::clear);
        self.allpasses.iter_mut().for_each(Allpass::clear);
    }

    fn tick(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let mut wet = 0.0;
        for comb in &mut self.combs {
            wet += comb.tick(input, feedback, damp);
        }
        for allpass in &mut self.allpasses {
            wet = allpass.tick(wet);
        }
        wet
    }
}

/// The reverb. Build it through [`INFO`] (or [`Reverb::new`] and [`Device::prepare`]).
#[derive(Debug, Clone)]
pub struct Reverb {
    sample_rate: f64,
    size: f64,
    damping: f64,
    mix: f64,
    left: Channel,
    right: Channel,
}

impl Reverb {
    /// A reverb with the default settings at 48 kHz.
    pub fn new() -> Self {
        let sample_rate = 48_000.0;
        Self {
            sample_rate,
            size: 50.0,
            damping: 50.0,
            mix: 25.0,
            left: Channel::new(sample_rate, 0),
            right: Channel::new(sample_rate, STEREO_SPREAD),
        }
    }
}

impl Default for Reverb {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for Reverb {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = if sample_rate.is_finite() && sample_rate >= 8_000.0 {
            f64::from(sample_rate)
        } else {
            48_000.0
        };
        self.left = Channel::new(self.sample_rate, 0);
        self.right = Channel::new(self.sample_rate, STEREO_SPREAD);
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let Some(descriptor) = PARAMS.iter().find(|p| p.id == id) else {
            return;
        };
        let value = descriptor.clamp(value);
        match id {
            PARAM_SIZE => self.size = value,
            PARAM_DAMPING => self.damping = value,
            PARAM_MIX => self.mix = value,
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.left.clear();
        self.right.clear();
    }

    fn process(&mut self, audio: &mut [&mut [f32]; 2], _events: &[Event]) {
        let [left, right] = audio;
        let feedback = (0.7 + 0.28 * self.size / 100.0) as f32;
        let damp = (0.4 * self.damping / 100.0) as f32;
        let wet = (self.mix / 100.0) as f32;
        let dry = 1.0 - wet;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let input = (*l + *r) * INPUT_GAIN;
            let wet_l = self.left.tick(input, feedback, damp);
            let wet_r = self.right.tick(input, feedback, damp);
            *l = *l * dry + wet_l * WET_SCALE * wet;
            *r = *r * dry + wet_r * WET_SCALE * wet;
        }
    }
}
