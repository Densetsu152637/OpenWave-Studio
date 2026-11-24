//! `builtin.compressor`: a feed-forward compressor with a hard knee.
//!
//! The detector is the larger of the two channels' instantaneous magnitudes (so a stereo image
//! does not shift), converted to decibels. A gain computer maps that level to a gain reduction:
//! above the threshold the output rises `1/ratio` decibels per input decibel. The reduction is
//! smoothed in the decibel domain by a one-pole filter that uses the attack time while the
//! reduction deepens and the release time while it recovers, then the makeup gain is added and
//! both channels are scaled by the same factor. There is no sidechain input and no lookahead, so
//! the device adds no latency.

use crate::device::{Device, DeviceInfo, Event, ParamDescriptor, Unit};

/// The `Device::kind` of the compressor.
pub const KIND: &str = "builtin.compressor";

/// Parameter id: threshold in decibels.
pub const PARAM_THRESHOLD: u32 = 0;
/// Parameter id: ratio, input decibels per output decibel above the threshold.
pub const PARAM_RATIO: u32 = 1;
/// Parameter id: attack time in seconds.
pub const PARAM_ATTACK: u32 = 2;
/// Parameter id: release time in seconds.
pub const PARAM_RELEASE: u32 = 3;
/// Parameter id: makeup gain in decibels.
pub const PARAM_MAKEUP: u32 = 4;

const PARAMS: &[ParamDescriptor] = &[
    ParamDescriptor::linear(
        PARAM_THRESHOLD,
        "Threshold",
        -60.0,
        0.0,
        -18.0,
        Unit::Decibels,
    ),
    ParamDescriptor::linear(PARAM_RATIO, "Ratio", 1.0, 20.0, 4.0, Unit::None),
    ParamDescriptor::log(PARAM_ATTACK, "Attack", 0.0001, 0.2, 0.01, Unit::Seconds),
    ParamDescriptor::log(PARAM_RELEASE, "Release", 0.01, 2.0, 0.15, Unit::Seconds),
    ParamDescriptor::linear(PARAM_MAKEUP, "Makeup", 0.0, 24.0, 0.0, Unit::Decibels),
];

/// Static description and constructor of the compressor.
pub const INFO: DeviceInfo = DeviceInfo::new(KIND, "Compressor", false, PARAMS, || {
    Box::new(Compressor::new())
});

/// Levels below this magnitude (-180 dB) are treated as silence by the detector.
const FLOOR: f32 = 1.0e-9;
/// `ln(10) / 20`: decibels to a natural-log gain.
const DB_TO_LN: f64 = std::f64::consts::LN_10 / 20.0;

/// The compressor. Build it through [`INFO`] (or [`Compressor::new`] and [`Device::prepare`]).
#[derive(Debug, Clone)]
pub struct Compressor {
    sample_rate: f64,
    threshold: f64,
    ratio: f64,
    attack: f64,
    release: f64,
    makeup: f64,
    attack_coef: f64,
    release_coef: f64,
    /// Smoothed gain change in decibels, zero or negative.
    reduction: f64,
}

impl Compressor {
    /// A compressor with the default settings at 48 kHz.
    pub fn new() -> Self {
        let mut compressor = Self {
            sample_rate: 48_000.0,
            threshold: -18.0,
            ratio: 4.0,
            attack: 0.01,
            release: 0.15,
            makeup: 0.0,
            attack_coef: 0.0,
            release_coef: 0.0,
            reduction: 0.0,
        };
        compressor.update_coefs();
        compressor
    }

    fn update_coefs(&mut self) {
        let coef = |seconds: f64| (-1.0 / (seconds * self.sample_rate)).exp();
        self.attack_coef = coef(self.attack);
        self.release_coef = coef(self.release);
    }
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for Compressor {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = if sample_rate.is_finite() && sample_rate >= 8_000.0 {
            f64::from(sample_rate)
        } else {
            48_000.0
        };
        self.update_coefs();
        self.reset();
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let Some(descriptor) = PARAMS.iter().find(|p| p.id == id) else {
            return;
        };
        let value = descriptor.clamp(value);
        match id {
            PARAM_THRESHOLD => self.threshold = value,
            PARAM_RATIO => self.ratio = value,
            PARAM_ATTACK => self.attack = value,
            PARAM_RELEASE => self.release = value,
            PARAM_MAKEUP => self.makeup = value,
            _ => return,
        }
        self.update_coefs();
    }

    fn reset(&mut self) {
        self.reduction = 0.0;
    }

    fn process(&mut self, audio: &mut [&mut [f32]; 2], _events: &[Event]) {
        let [left, right] = audio;
        let slope = 1.0 - 1.0 / self.ratio;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let level = l.abs().max(r.abs()).max(FLOOR);
            let level_db = 20.0 * f64::from(level).log10();
            let over = level_db - self.threshold;
            let target = if over > 0.0 { -over * slope } else { 0.0 };
            let coef = if target < self.reduction {
                self.attack_coef
            } else {
                self.release_coef
            };
            self.reduction = target + coef * (self.reduction - target);
            if self.reduction > -1.0e-9 {
                self.reduction = 0.0;
            }
            let gain = ((self.reduction + self.makeup) * DB_TO_LN).exp() as f32;
            *l *= gain;
            *r *= gain;
        }
    }
}
