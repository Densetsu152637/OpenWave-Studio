//! Building blocks the built-in instruments share: envelopes, the state-variable filter,
//! band-limited oscillator pieces, noise and voice stealing. Everything here is allocation-free.

/// Voices both instruments play at once.
pub const VOICES: usize = 16;

/// Frames between control-rate updates (filter coefficients, modulation, parameter glides). The
/// update points fall on multiples of this count of frames since the last reset, whatever the
/// block sizes and event positions, so a render does not depend on how it is cut into blocks.
pub const CONTROL: usize = 16;

/// Envelope level below which a releasing voice is freed (-100 dB).
pub const SILENT: f32 = 1.0e-5;

/// Largest oscillator phase step (cycles per sample); keeps PolyBLEP's two-sample window valid.
pub const MAX_STEP: f64 = 0.45;

/// Frequency in hertz of the (fractional) MIDI note `note`.
pub fn note_hz(note: f64) -> f64 {
    440.0 * ((note - 69.0) / 12.0).exp2()
}

/// A gain factor from decibels.
pub fn db_to_gain(db: f64) -> f32 {
    10.0_f64.powf(db / 20.0) as f32
}

/// A deterministic xorshift generator for noise and sample-and-hold.
#[derive(Debug, Clone, Copy)]
pub struct Rng(u32);

impl Rng {
    /// A generator from `seed` (0 is replaced by a fixed non-zero value).
    pub const fn new(seed: u32) -> Self {
        Self(if seed == 0 { 0x9E37_79B9 } else { seed })
    }

    /// The next value, from -1 (inclusive) to 1 (exclusive).
    pub fn next_bipolar(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / 2_147_483_648.0) - 1.0
    }
}

/// Where in its cycle an envelope is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Rising linearly to 1.
    Attack,
    /// Falling exponentially toward the sustain level.
    Decay,
    /// Holding the sustain level.
    Sustain,
    /// Falling exponentially toward silence.
    Release,
}

/// ADSR timing as per-sample coefficients. The decay and release times are the time to fall 60 dB.
#[derive(Debug, Clone, Copy, Default)]
pub struct EnvCoefs {
    attack_inc: f32,
    decay: f32,
    release: f32,
    sustain: f32,
}

impl EnvCoefs {
    /// Coefficients for times in seconds (each at least a millisecond) and a sustain level.
    pub fn new(attack: f64, decay: f64, sustain: f64, release: f64, sample_rate: f64) -> Self {
        let ln_1000 = 1000.0_f64.ln();
        Self {
            attack_inc: (1.0 / (attack.max(1.0e-4) * sample_rate)).min(1.0) as f32,
            decay: (-ln_1000 / (decay.max(1.0e-4) * sample_rate)).exp() as f32,
            release: (-ln_1000 / (release.max(1.0e-4) * sample_rate)).exp() as f32,
            sustain: sustain as f32,
        }
    }
}

/// An ADSR envelope generator: linear attack, exponential decay and release.
#[derive(Debug, Clone, Copy)]
pub struct Envelope {
    /// Current stage.
    pub stage: Stage,
    /// Current level, 0 to 1.
    pub level: f32,
}

impl Envelope {
    /// A silent, released envelope.
    pub const fn idle() -> Self {
        Self {
            stage: Stage::Release,
            level: 0.0,
        }
    }

    /// Starts the attack from the current level (a stolen voice does not click).
    pub fn trigger(&mut self) {
        self.stage = Stage::Attack;
    }

    /// Starts the release.
    pub fn release(&mut self) {
        self.stage = Stage::Release;
    }

    /// Advances one sample and returns the new level.
    #[inline]
    pub fn tick(&mut self, c: &EnvCoefs) -> f32 {
        match self.stage {
            Stage::Attack => {
                self.level += c.attack_inc;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level = c.sustain + (self.level - c.sustain) * c.decay;
                if (self.level - c.sustain).abs() < 1.0e-6 {
                    self.level = c.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = c.sustain,
            Stage::Release => self.level *= c.release,
        }
        self.level
    }

    /// True once a release has faded below [`SILENT`].
    pub fn finished(&self) -> bool {
        self.stage == Stage::Release && self.level < SILENT
    }
}

/// Which output of the filter to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    /// Low-pass.
    LowPass,
    /// High-pass.
    HighPass,
    /// Band-pass.
    BandPass,
}

/// Coefficients of a trapezoidal-integration (TPT) state-variable filter, stable at any cutoff
/// below Nyquist.
#[derive(Debug, Clone, Copy, Default)]
pub struct SvfCoefs {
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32,
}

impl SvfCoefs {
    /// Placeholder coefficients for an idle voice; replaced before the voice sounds.
    pub const fn flat() -> Self {
        Self {
            a1: 0.0,
            a2: 0.0,
            a3: 0.0,
            k: 0.0,
        }
    }

    /// Coefficients for `cutoff` hertz and quality `q` at `sample_rate`.
    pub fn new(cutoff: f64, q: f64, sample_rate: f64) -> Self {
        let k = 1.0 / q;
        let cutoff = cutoff.clamp(10.0, sample_rate * 0.45);
        let g = (std::f64::consts::PI * cutoff / sample_rate).tan();
        let a1 = 1.0 / (1.0 + g * (g + k));
        Self {
            a1: a1 as f32,
            a2: (g * a1) as f32,
            a3: (g * g * a1) as f32,
            k: k as f32,
        }
    }
}

/// State of one filter channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
}

impl Svf {
    /// A cleared filter.
    pub const fn new() -> Self {
        Self { ic1: 0.0, ic2: 0.0 }
    }

    /// Filters one sample.
    #[inline]
    pub fn tick(&mut self, c: &SvfCoefs, x: f32, mode: FilterMode) -> f32 {
        let v3 = x - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        match mode {
            FilterMode::LowPass => v2,
            FilterMode::BandPass => v1,
            FilterMode::HighPass => x - c.k * v1 - v2,
        }
    }
}

/// The PolyBLEP residual around a discontinuity at phase 0: `t` is the phase, `dt` the step.
pub fn poly_blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// The PolyBLAMP residual (the integral of PolyBLEP) around a slope change at phase 0, for a
/// unit change of slope per cycle; scale by `dt`.
pub fn poly_blamp(t: f64, dt: f64) -> f64 {
    if t < dt {
        let x = t / dt - 1.0;
        -x * x * x / 3.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt + 1.0;
        x * x * x / 3.0
    } else {
        0.0
    }
}

/// A free slot's summary for [`pick_voice`].
#[derive(Debug, Clone, Copy)]
pub struct Slot {
    /// The voice is sounding.
    pub active: bool,
    /// The note is held (no note-off yet).
    pub gate: bool,
    /// The voice's current amplitude.
    pub level: f32,
    /// Note-on counter value, to find the oldest.
    pub age: u64,
}

/// The voice a note-on takes: a free one; else the quietest releasing one; else the oldest.
pub fn pick_voice(slots: &[Slot]) -> usize {
    if let Some(free) = slots.iter().position(|s| !s.active) {
        return free;
    }
    let releasing = slots
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.gate)
        .min_by(|a, b| a.1.level.total_cmp(&b.1.level))
        .map(|(i, _)| i);
    releasing.unwrap_or_else(|| {
        slots
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| s.age)
            .map_or(0, |(i, _)| i)
    })
}
