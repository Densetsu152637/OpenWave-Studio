//! `builtin.synth`: a small subtractive synthesizer.
//!
//! One oscillator (saw, square or sine), one resonant low-pass filter and one ADSR amplitude
//! envelope per voice, 16 voices, and a master gain. The saw and square are band-limited with
//! PolyBLEP (a two-sample polynomial correction at each discontinuity), which keeps aliasing
//! low without wavetables; the filter is a trapezoidal-integration state-variable filter (stable
//! at any cutoff below Nyquist). Every voice makes the same signal for both channels.
//!
//! Voices: a note-on takes a free voice, else steals the quietest releasing voice, else the
//! oldest. The new note continues from the stolen voice's envelope level, so a steal does not
//! click. A note-off releases every held voice on that key. Envelope segments: linear attack,
//! then exponential decay toward the sustain level and exponential release toward silence (the
//! decay and release times are the time to fall 60 dB). A voice is freed, and its filter
//! cleared, once its release falls below -100 dB, after which it produces exact zeros.
//!
//! Parameters change at block boundaries and, within a block, at once; there is no smoothing
//! because the app applies edits by rebuilding the device between blocks.

use std::f64::consts::TAU;

use crate::device::{Device, DeviceInfo, Event, EventKind, ParamDescriptor, Unit};

/// The `Device::kind` of the synth.
pub const KIND: &str = "builtin.synth";

/// Parameter id: oscillator waveform (0 saw, 1 square, 2 sine).
pub const PARAM_WAVEFORM: u32 = 0;
/// Parameter id: low-pass cutoff in hertz.
pub const PARAM_CUTOFF: u32 = 1;
/// Parameter id: filter resonance, 0 (none) to 1 (strong).
pub const PARAM_RESONANCE: u32 = 2;
/// Parameter id: envelope attack time in seconds.
pub const PARAM_ATTACK: u32 = 3;
/// Parameter id: envelope decay time in seconds.
pub const PARAM_DECAY: u32 = 4;
/// Parameter id: envelope sustain level, 0 to 1.
pub const PARAM_SUSTAIN: u32 = 5;
/// Parameter id: envelope release time in seconds.
pub const PARAM_RELEASE: u32 = 6;
/// Parameter id: output gain in decibels.
pub const PARAM_GAIN: u32 = 7;

/// Waveform choice labels, in index order.
pub const WAVEFORMS: &[&str] = &["Saw", "Square", "Sine"];

/// Simultaneous voices.
pub const VOICES: usize = 16;

const PARAMS: &[ParamDescriptor] = &[
    ParamDescriptor::choice(PARAM_WAVEFORM, "Waveform", WAVEFORMS, 0),
    ParamDescriptor::log(PARAM_CUTOFF, "Cutoff", 20.0, 20_000.0, 8_000.0, Unit::Hertz),
    ParamDescriptor::linear(PARAM_RESONANCE, "Resonance", 0.0, 1.0, 0.1, Unit::None),
    ParamDescriptor::log(PARAM_ATTACK, "Attack", 0.001, 5.0, 0.01, Unit::Seconds),
    ParamDescriptor::log(PARAM_DECAY, "Decay", 0.001, 5.0, 0.2, Unit::Seconds),
    ParamDescriptor::linear(PARAM_SUSTAIN, "Sustain", 0.0, 1.0, 0.7, Unit::None),
    ParamDescriptor::log(PARAM_RELEASE, "Release", 0.001, 10.0, 0.3, Unit::Seconds),
    ParamDescriptor::linear(PARAM_GAIN, "Gain", -60.0, 6.0, -9.0, Unit::Decibels),
];

/// Static description and constructor of the synth.
pub const INFO: DeviceInfo =
    DeviceInfo::new(KIND, "Synth", true, PARAMS, || Box::new(Synth::new()));

/// Envelope level below which a releasing voice is freed (-100 dB).
const SILENT: f32 = 1.0e-5;
/// Largest oscillator phase step (cycles per sample); keeps PolyBLEP's two-sample window valid.
const MAX_STEP: f64 = 0.45;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Debug, Clone, Copy)]
struct Voice {
    active: bool,
    /// The note is held (no note-off yet).
    gate: bool,
    key: u8,
    /// Note-on counter value, for stealing the oldest.
    age: u64,
    phase: f64,
    step: f64,
    stage: Stage,
    level: f32,
    velocity: f32,
    ic1: f32,
    ic2: f32,
}

impl Voice {
    const fn idle() -> Self {
        Self {
            active: false,
            gate: false,
            key: 0,
            age: 0,
            phase: 0.0,
            step: 0.0,
            stage: Stage::Release,
            level: 0.0,
            velocity: 0.0,
            ic1: 0.0,
            ic2: 0.0,
        }
    }
}

/// Coefficients that depend on parameters and the sample rate.
#[derive(Debug, Clone, Copy)]
struct Coefs {
    attack_inc: f32,
    decay: f32,
    release: f32,
    sustain: f32,
    // State-variable filter (Zavalishin's trapezoidal form).
    a1: f32,
    a2: f32,
    a3: f32,
    gain: f32,
}

/// The synth. Build it through [`INFO`] (or [`Synth::new`] and [`Device::prepare`]).
#[derive(Debug, Clone)]
pub struct Synth {
    sample_rate: f32,
    waveform: u8,
    cutoff: f64,
    resonance: f64,
    attack: f64,
    decay: f64,
    sustain: f64,
    release: f64,
    gain_db: f64,
    coefs: Coefs,
    voices: [Voice; VOICES],
    counter: u64,
}

impl Default for Synth {
    fn default() -> Self {
        Self::new()
    }
}

impl Synth {
    /// A synth with default parameters at 48 kHz; call [`Device::prepare`] for the real rate.
    pub fn new() -> Self {
        let defaults = |id| {
            PARAMS
                .iter()
                .find(|p| p.id == id)
                .map_or(0.0, |p| p.default)
        };
        let mut synth = Self {
            sample_rate: 48_000.0,
            waveform: defaults(PARAM_WAVEFORM) as u8,
            cutoff: defaults(PARAM_CUTOFF),
            resonance: defaults(PARAM_RESONANCE),
            attack: defaults(PARAM_ATTACK),
            decay: defaults(PARAM_DECAY),
            sustain: defaults(PARAM_SUSTAIN),
            release: defaults(PARAM_RELEASE),
            gain_db: defaults(PARAM_GAIN),
            coefs: Coefs {
                attack_inc: 0.0,
                decay: 0.0,
                release: 0.0,
                sustain: 0.0,
                a1: 0.0,
                a2: 0.0,
                a3: 0.0,
                gain: 0.0,
            },
            voices: [Voice::idle(); VOICES],
            counter: 0,
        };
        synth.update_coefs();
        synth
    }

    /// Number of voices currently producing sound.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    fn update_coefs(&mut self) {
        let sr = f64::from(self.sample_rate);
        let ln_1000 = 1000.0_f64.ln();
        // Envelope.
        self.coefs.attack_inc = (1.0 / (self.attack * sr)).min(1.0) as f32;
        self.coefs.decay = (-ln_1000 / (self.decay * sr)).exp() as f32;
        self.coefs.release = (-ln_1000 / (self.release * sr)).exp() as f32;
        self.coefs.sustain = self.sustain as f32;
        // Filter: Q from 0.707 (flat) up to about 11 at full resonance.
        let q = std::f64::consts::FRAC_1_SQRT_2 * (1.0 + self.resonance * 15.0);
        let k = 1.0 / q;
        let cutoff = self.cutoff.clamp(10.0, sr * 0.45);
        let g = (std::f64::consts::PI * cutoff / sr).tan();
        let a1 = 1.0 / (1.0 + g * (g + k));
        self.coefs.a1 = a1 as f32;
        self.coefs.a2 = (g * a1) as f32;
        self.coefs.a3 = (g * g * a1) as f32;
        self.coefs.gain = 10.0_f64.powf(self.gain_db / 20.0) as f32;
    }

    fn note_on(&mut self, key: u8, velocity: u8) {
        self.counter += 1;
        let index = self.pick_voice();
        let sr = f64::from(self.sample_rate);
        let freq = 440.0 * 2.0_f64.powf((f64::from(key) - 69.0) / 12.0);
        let voice = &mut self.voices[index];
        if !voice.active {
            voice.phase = 0.0;
            voice.level = 0.0;
            voice.ic1 = 0.0;
            voice.ic2 = 0.0;
        }
        voice.active = true;
        voice.gate = true;
        voice.key = key;
        voice.age = self.counter;
        voice.step = (freq / sr).min(MAX_STEP);
        voice.stage = Stage::Attack;
        voice.velocity = f32::from(velocity.clamp(1, 127)) / 127.0;
    }

    /// A free voice; else the quietest releasing one; else the oldest.
    fn pick_voice(&self) -> usize {
        if let Some(free) = self.voices.iter().position(|v| !v.active) {
            return free;
        }
        let releasing = self
            .voices
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.gate)
            .min_by(|a, b| a.1.level.total_cmp(&b.1.level))
            .map(|(i, _)| i);
        releasing.unwrap_or_else(|| {
            self.voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.age)
                .map_or(0, |(i, _)| i)
        })
    }

    fn note_off(&mut self, key: u8) {
        for voice in &mut self.voices {
            if voice.active && voice.gate && voice.key == key {
                voice.gate = false;
                voice.stage = Stage::Release;
            }
        }
    }

    fn apply(&mut self, event: &Event) {
        match event.kind {
            EventKind::NoteOn { key, velocity } => self.note_on(key, velocity),
            EventKind::NoteOff { key } => self.note_off(key),
        }
    }

    /// Adds the voices' output for `out` (one channel's worth of frames) into it.
    fn render(&mut self, out: &mut [f32]) {
        let c = self.coefs;
        let waveform = self.waveform;
        for voice in self.voices.iter_mut().filter(|v| v.active) {
            for sample in out.iter_mut() {
                // The phase advances first, so a note's first sample is already non-zero.
                voice.phase += voice.step;
                if voice.phase >= 1.0 {
                    voice.phase -= 1.0;
                }
                let osc = oscillator(waveform, voice.phase, voice.step);
                match voice.stage {
                    Stage::Attack => {
                        voice.level += c.attack_inc;
                        if voice.level >= 1.0 {
                            voice.level = 1.0;
                            voice.stage = Stage::Decay;
                        }
                    }
                    Stage::Decay => {
                        voice.level = c.sustain + (voice.level - c.sustain) * c.decay;
                        if (voice.level - c.sustain).abs() < 1.0e-6 {
                            voice.level = c.sustain;
                            voice.stage = Stage::Sustain;
                        }
                    }
                    Stage::Sustain => voice.level = c.sustain,
                    Stage::Release => voice.level *= c.release,
                }
                // Trapezoidal state-variable low-pass.
                let v3 = osc - voice.ic2;
                let v1 = c.a1 * voice.ic1 + c.a2 * v3;
                let v2 = voice.ic2 + c.a2 * voice.ic1 + c.a3 * v3;
                voice.ic1 = 2.0 * v1 - voice.ic1;
                voice.ic2 = 2.0 * v2 - voice.ic2;
                *sample += v2 * voice.level * voice.velocity;
                if voice.stage == Stage::Release && voice.level < SILENT {
                    *voice = Voice::idle();
                    break;
                }
            }
        }
    }
}

/// One oscillator sample at `phase` (0 to 1) advancing by `step` cycles per sample.
fn oscillator(waveform: u8, phase: f64, step: f64) -> f32 {
    let value = match waveform {
        0 => 2.0 * phase - 1.0 - poly_blep(phase, step),
        1 => {
            let naive = if phase < 0.5 { 1.0 } else { -1.0 };
            naive + poly_blep(phase, step) - poly_blep((phase + 0.5).fract(), step)
        }
        _ => (phase * TAU).sin(),
    };
    value as f32
}

/// The PolyBLEP residual around a discontinuity at phase 0: `t` is the phase, `dt` the step.
fn poly_blep(t: f64, dt: f64) -> f64 {
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

impl Device for Synth {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = if sample_rate.is_finite() && sample_rate >= 8_000.0 {
            sample_rate
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
            PARAM_WAVEFORM => self.waveform = value as u8,
            PARAM_CUTOFF => self.cutoff = value,
            PARAM_RESONANCE => self.resonance = value,
            PARAM_ATTACK => self.attack = value,
            PARAM_DECAY => self.decay = value,
            PARAM_SUSTAIN => self.sustain = value,
            PARAM_RELEASE => self.release = value,
            PARAM_GAIN => self.gain_db = value,
            _ => return,
        }
        self.update_coefs();
    }

    fn reset(&mut self) {
        self.voices = [Voice::idle(); VOICES];
    }

    fn process(&mut self, audio: &mut [&mut [f32]; 2], events: &[Event]) {
        let [left, right] = audio;
        let frames = left.len().min(right.len());
        left[..frames].fill(0.0);
        let mut position = 0;
        let mut next = 0;
        while position < frames {
            while let Some(event) = events.get(next)
                && event.offset as usize <= position
            {
                self.apply(event);
                next += 1;
            }
            let end = events
                .get(next)
                .map_or(frames, |event| (event.offset as usize).min(frames));
            self.render(&mut left[position..end]);
            position = end;
        }
        let gain = self.coefs.gain;
        for (l, r) in left[..frames].iter_mut().zip(right[..frames].iter_mut()) {
            *l *= gain;
            *r = *l;
        }
    }
}
