//! `builtin.eq`: a three-band equalizer, low shelf, peak and high shelf.
//!
//! Each band is one biquad from Robert Bristow-Johnson's Audio EQ Cookbook, run in transposed
//! direct form II with `f64` coefficients and state so low frequencies stay accurate. A band
//! with no gain is skipped. The peak band reaches exactly its gain at its centre frequency; a
//! shelf reaches half its gain (in decibels) at its corner frequency and its full gain a few
//! octaves beyond it. Parameters take effect at once, with no smoothing, because the app applies
//! edits by rebuilding the device between blocks.

use std::f64::consts::TAU;

use crate::device::{Device, DeviceInfo, Event, ParamDescriptor, Unit};

/// The `Device::kind` of the equalizer.
pub const KIND: &str = "builtin.eq";

/// Parameter id: low shelf corner frequency in hertz.
pub const PARAM_LOW_FREQ: u32 = 0;
/// Parameter id: low shelf gain in decibels.
pub const PARAM_LOW_GAIN: u32 = 1;
/// Parameter id: low shelf quality.
pub const PARAM_LOW_Q: u32 = 2;
/// Parameter id: peak centre frequency in hertz.
pub const PARAM_MID_FREQ: u32 = 3;
/// Parameter id: peak gain in decibels.
pub const PARAM_MID_GAIN: u32 = 4;
/// Parameter id: peak quality.
pub const PARAM_MID_Q: u32 = 5;
/// Parameter id: high shelf corner frequency in hertz.
pub const PARAM_HIGH_FREQ: u32 = 6;
/// Parameter id: high shelf gain in decibels.
pub const PARAM_HIGH_GAIN: u32 = 7;
/// Parameter id: high shelf quality.
pub const PARAM_HIGH_Q: u32 = 8;

const PARAMS: &[ParamDescriptor] = &[
    ParamDescriptor::log(
        PARAM_LOW_FREQ,
        "Low freq",
        20.0,
        2_000.0,
        100.0,
        Unit::Hertz,
    ),
    ParamDescriptor::linear(PARAM_LOW_GAIN, "Low gain", -18.0, 18.0, 0.0, Unit::Decibels),
    ParamDescriptor::log(PARAM_LOW_Q, "Low Q", 0.1, 10.0, 0.707, Unit::None),
    ParamDescriptor::log(
        PARAM_MID_FREQ,
        "Mid freq",
        100.0,
        10_000.0,
        1_000.0,
        Unit::Hertz,
    ),
    ParamDescriptor::linear(PARAM_MID_GAIN, "Mid gain", -18.0, 18.0, 0.0, Unit::Decibels),
    ParamDescriptor::log(PARAM_MID_Q, "Mid Q", 0.1, 10.0, 1.0, Unit::None),
    ParamDescriptor::log(
        PARAM_HIGH_FREQ,
        "High freq",
        1_000.0,
        20_000.0,
        8_000.0,
        Unit::Hertz,
    ),
    ParamDescriptor::linear(
        PARAM_HIGH_GAIN,
        "High gain",
        -18.0,
        18.0,
        0.0,
        Unit::Decibels,
    ),
    ParamDescriptor::log(PARAM_HIGH_Q, "High Q", 0.1, 10.0, 0.707, Unit::None),
];

/// Static description and constructor of the equalizer.
pub const INFO: DeviceInfo = DeviceInfo::new(KIND, "EQ", false, PARAMS, || Box::new(Eq::new()));

/// Gains closer to zero than this (in decibels) leave the band out of the chain.
const FLAT_DB: f64 = 1.0e-3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    LowShelf,
    Peak,
    HighShelf,
}

/// Normalized biquad coefficients (`a0` divided out).
#[derive(Debug, Clone, Copy)]
struct Coefs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coefs {
    const UNITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// RBJ cookbook coefficients for `shape` (quality form of the shelves).
    fn design(shape: Shape, sample_rate: f64, freq: f64, gain_db: f64, q: f64) -> Self {
        let freq = freq.min(sample_rate * 0.45);
        let w0 = TAU * freq / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a = 10.0_f64.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match shape {
            Shape::Peak => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            Shape::LowShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + k),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - k),
                    (a + 1.0) + (a - 1.0) * cos + k,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - k,
                )
            }
            Shape::HighShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + k),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - k),
                    (a + 1.0) - (a - 1.0) * cos + k,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - k,
                )
            }
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }
}

/// One band: its settings, coefficients and the state of both channels.
#[derive(Debug, Clone, Copy)]
struct Band {
    shape: Shape,
    freq: f64,
    gain_db: f64,
    q: f64,
    coefs: Coefs,
    active: bool,
    /// Transposed direct form II state, `[channel][z1, z2]`.
    state: [[f64; 2]; 2],
}

impl Band {
    fn new(shape: Shape, freq: f64, q: f64) -> Self {
        Self {
            shape,
            freq,
            gain_db: 0.0,
            q,
            coefs: Coefs::UNITY,
            active: false,
            state: [[0.0; 2]; 2],
        }
    }

    fn update(&mut self, sample_rate: f64) {
        self.active = self.gain_db.abs() > FLAT_DB;
        self.coefs = if self.active {
            Coefs::design(self.shape, sample_rate, self.freq, self.gain_db, self.q)
        } else {
            Coefs::UNITY
        };
    }

    fn run(&mut self, channel: usize, samples: &mut [f32]) {
        let c = self.coefs;
        let [mut z1, mut z2] = self.state[channel];
        for sample in samples {
            let x = f64::from(*sample);
            let y = c.b0 * x + z1;
            z1 = c.b1 * x - c.a1 * y + z2;
            z2 = c.b2 * x - c.a2 * y;
            *sample = y as f32;
        }
        // Flush values that would decay into denormals.
        let flush = |z: f64| if z.abs() < 1.0e-30 { 0.0 } else { z };
        self.state[channel] = [flush(z1), flush(z2)];
    }
}

/// The equalizer. Build it through [`INFO`] (or [`Eq::new`] and [`Device::prepare`]).
#[derive(Debug, Clone)]
pub struct Eq {
    sample_rate: f64,
    bands: [Band; 3],
}

impl Eq {
    /// A flat equalizer at 48 kHz.
    pub fn new() -> Self {
        Self {
            sample_rate: 48_000.0,
            bands: [
                Band::new(Shape::LowShelf, 100.0, 0.707),
                Band::new(Shape::Peak, 1_000.0, 1.0),
                Band::new(Shape::HighShelf, 8_000.0, 0.707),
            ],
        }
    }

    fn update_all(&mut self) {
        let rate = self.sample_rate;
        for band in &mut self.bands {
            band.update(rate);
        }
    }
}

impl Default for Eq {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for Eq {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = if sample_rate.is_finite() && sample_rate >= 8_000.0 {
            f64::from(sample_rate)
        } else {
            48_000.0
        };
        self.update_all();
        self.reset();
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let Some(descriptor) = PARAMS.iter().find(|p| p.id == id) else {
            return;
        };
        let value = descriptor.clamp(value);
        let band = &mut self.bands[(id / 3) as usize];
        match id % 3 {
            0 => band.freq = value,
            1 => band.gain_db = value,
            _ => band.q = value,
        }
        band.update(self.sample_rate);
    }

    fn reset(&mut self) {
        for band in &mut self.bands {
            band.state = [[0.0; 2]; 2];
        }
    }

    fn process(&mut self, audio: &mut [&mut [f32]; 2], _events: &[Event]) {
        for band in self.bands.iter_mut().filter(|b| b.active) {
            for (channel, samples) in audio.iter_mut().enumerate() {
                band.run(channel, samples);
            }
        }
    }
}
