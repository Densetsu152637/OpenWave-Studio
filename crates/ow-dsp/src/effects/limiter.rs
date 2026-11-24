//! `builtin.limiter`: a lookahead brickwall limiter.
//!
//! The signal is delayed by the lookahead (1 to 5 ms) while a gain curve is computed from what is
//! about to arrive. For every input frame the wanted gain is `ceiling / peak` (the larger of the
//! two channels' magnitudes, so the stereo image does not shift) or 1 below the ceiling. The
//! curve is the minimum of the wanted gain over the last `lookahead + 1` frames (so the reduction
//! is in place before the peak reaches the output), smoothed by a moving average over `lookahead`
//! frames (so it ramps instead of stepping), then recovers with a one-pole release. Every stage
//! can only keep the gain at or below the wanted one, and the output is finally clamped to the
//! ceiling, so it never exceeds it on a sample-peak basis. Intersample (true) peaks can still
//! exceed it by a fraction of a decibel, so mastering chains leave a little room.
//!
//! The device reports the lookahead as its latency ([`Device::latency_frames`]). All buffers are
//! sized in `prepare` for the longest lookahead; changing parameters never allocates.

use crate::device::{Device, DeviceInfo, Event, ParamDescriptor, Unit};

/// The `Device::kind` of the limiter.
pub const KIND: &str = "builtin.limiter";

/// Parameter id: output ceiling in decibels (full scale).
pub const PARAM_CEILING: u32 = 0;
/// Parameter id: release time in seconds.
pub const PARAM_RELEASE: u32 = 1;
/// Parameter id: lookahead in seconds (also the reported latency).
pub const PARAM_LOOKAHEAD: u32 = 2;

const PARAMS: &[ParamDescriptor] = &[
    ParamDescriptor::linear(PARAM_CEILING, "Ceiling", -20.0, 0.0, -1.0, Unit::Decibels),
    ParamDescriptor::log(PARAM_RELEASE, "Release", 0.01, 1.0, 0.1, Unit::Seconds),
    ParamDescriptor::linear(
        PARAM_LOOKAHEAD,
        "Lookahead",
        0.001,
        0.005,
        0.002,
        Unit::Seconds,
    ),
];

/// Static description and constructor of the limiter.
pub const INFO: DeviceInfo =
    DeviceInfo::new(KIND, "Limiter", false, PARAMS, || Box::new(Limiter::new()));

/// Longest lookahead in seconds (the top of the parameter range).
const MAX_LOOKAHEAD: f64 = 0.005;

/// The limiter. Build it through [`INFO`] (or [`Limiter::new`] and [`Device::prepare`]).
#[derive(Debug, Clone)]
pub struct Limiter {
    sample_rate: f64,
    ceiling_db: f64,
    release: f64,
    lookahead_seconds: f64,
    /// Lookahead in frames (at least 1); zero until `prepare` has sized the buffers.
    lookahead: usize,
    ceiling: f32,
    release_coef: f32,
    /// Delay lines of `capacity` frames; `write` is the next slot.
    delay: [Vec<f32>; 2],
    /// Monotonic queue for the sliding minimum: values and the frame index each was pushed at.
    min_value: Vec<f32>,
    min_index: Vec<u64>,
    min_head: usize,
    min_len: usize,
    /// The last `lookahead` held minima (a ring indexed by frame count) and their sum.
    hold: Vec<f32>,
    hold_sum: f64,
    write: usize,
    /// Frames processed since the last reset.
    count: u64,
    /// The released gain.
    envelope: f32,
}

impl Limiter {
    /// A limiter with the default settings at 48 kHz. It passes audio through until
    /// [`Device::prepare`] has sized its buffers.
    pub fn new() -> Self {
        Self {
            sample_rate: 48_000.0,
            ceiling_db: -1.0,
            release: 0.1,
            lookahead_seconds: 0.002,
            lookahead: 0,
            ceiling: 0.891_250_9,
            release_coef: 0.0,
            delay: [Vec::new(), Vec::new()],
            min_value: Vec::new(),
            min_index: Vec::new(),
            min_head: 0,
            min_len: 0,
            hold: Vec::new(),
            hold_sum: 0.0,
            write: 0,
            count: 0,
            envelope: 1.0,
        }
    }

    fn capacity(&self) -> usize {
        self.delay[0].len()
    }

    fn update(&mut self) {
        self.ceiling = 10.0_f64.powf(self.ceiling_db / 20.0) as f32;
        self.release_coef = (-1.0 / (self.release * self.sample_rate)).exp() as f32;
        let capacity = self.capacity();
        if capacity > 0 {
            let wanted = (self.lookahead_seconds * self.sample_rate).round() as usize;
            let lookahead = wanted.clamp(1, capacity - 1);
            if lookahead != self.lookahead {
                self.lookahead = lookahead;
                self.reset();
            }
        }
    }
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for Limiter {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = if sample_rate.is_finite() && sample_rate >= 8_000.0 {
            f64::from(sample_rate)
        } else {
            48_000.0
        };
        // One frame more than the longest lookahead for the frame being written, and one for
        // rounding.
        let capacity = (MAX_LOOKAHEAD * self.sample_rate).ceil() as usize + 2;
        for line in &mut self.delay {
            *line = vec![0.0; capacity];
        }
        self.min_value = vec![0.0; capacity];
        self.min_index = vec![0; capacity];
        self.hold = vec![1.0; capacity];
        self.lookahead = 0;
        self.update();
        self.reset();
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let Some(descriptor) = PARAMS.iter().find(|p| p.id == id) else {
            return;
        };
        let value = descriptor.clamp(value);
        match id {
            PARAM_CEILING => self.ceiling_db = value,
            PARAM_RELEASE => self.release = value,
            PARAM_LOOKAHEAD => self.lookahead_seconds = value,
            _ => return,
        }
        self.update();
    }

    fn reset(&mut self) {
        for line in &mut self.delay {
            line.fill(0.0);
        }
        self.hold.fill(1.0);
        self.hold_sum = self.lookahead as f64;
        self.min_head = 0;
        self.min_len = 0;
        self.write = 0;
        self.count = 0;
        self.envelope = 1.0;
    }

    fn latency_frames(&self) -> u32 {
        self.lookahead as u32
    }

    fn process(&mut self, audio: &mut [&mut [f32]; 2], _events: &[Event]) {
        let capacity = self.capacity();
        if capacity == 0 || self.lookahead == 0 {
            return;
        }
        let lookahead = self.lookahead;
        let [left, right] = audio;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (inl, inr) = (*l, *r);
            let peak = inl.abs().max(inr.abs());
            let wanted = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };

            // Sliding minimum of `wanted` over the last `lookahead + 1` frames.
            let index = self.count;
            while self.min_len > 0 {
                let back = (self.min_head + self.min_len - 1) % capacity;
                if self.min_value[back] >= wanted {
                    self.min_len -= 1;
                } else {
                    break;
                }
            }
            let slot = (self.min_head + self.min_len) % capacity;
            self.min_value[slot] = wanted;
            self.min_index[slot] = index;
            self.min_len += 1;
            while self.min_index[self.min_head] + (lookahead as u64) < index {
                self.min_head = (self.min_head + 1) % capacity;
                self.min_len -= 1;
            }
            let held = self.min_value[self.min_head];

            // Moving average of the held minimum over the last `lookahead` frames.
            let ring = (index % lookahead as u64) as usize;
            self.hold_sum += f64::from(held) - f64::from(self.hold[ring]);
            self.hold[ring] = held;
            let smoothed = (self.hold_sum / lookahead as f64) as f32;

            // Release: follow the curve down at once, recover with the one-pole.
            self.envelope = if smoothed < self.envelope {
                smoothed
            } else {
                self.release_coef * self.envelope + (1.0 - self.release_coef) * smoothed
            };

            // The delayed frame, scaled; the clamp makes the ceiling exact despite rounding.
            let read = (self.write + capacity - lookahead) % capacity;
            let (dl, dr) = (self.delay[0][read], self.delay[1][read]);
            self.delay[0][self.write] = inl;
            self.delay[1][self.write] = inr;
            let gain = self.envelope;
            *l = (dl * gain).clamp(-self.ceiling, self.ceiling);
            *r = (dr * gain).clamp(-self.ceiling, self.ceiling);

            self.write += 1;
            if self.write == capacity {
                self.write = 0;
            }
            self.count += 1;
        }
    }
}
