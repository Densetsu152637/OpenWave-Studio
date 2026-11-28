//! `builtin.synth`: a subtractive synthesizer.
//!
//! Per voice: two oscillators (saw, square, sine or triangle, each with octave, semitone and fine
//! tuning and a level), a sub oscillator (sine or square, one or two octaves down), a noise
//! source, a resonant filter (low-pass, high-pass or band-pass; a trapezoidal state-variable
//! filter that is stable at any cutoff below Nyquist) with its own ADSR envelope, key tracking and
//! LFO modulation, an ADSR amplitude envelope, and a unison stack (1 to 7 detuned copies of each
//! oscillator, spread across the stereo field). 16 voices, one master gain.
//!
//! The saw, square and triangle are band-limited with PolyBLEP and PolyBLAMP (a two-sample
//! polynomial correction at each discontinuity or slope change), which keeps aliasing low without
//! wavetables.
//!
//! Voices: a note-on takes a free voice, else steals the quietest releasing voice, else the
//! oldest. The new note continues from the stolen voice's envelope level, so a steal does not
//! click. A note-off releases every held voice on that key. Envelope segments: linear attack,
//! then exponential decay toward the sustain level and exponential release toward silence (the
//! decay and release times are the time to fall 60 dB). A voice is freed, and its filter
//! cleared, once its release falls below -100 dB, after which it produces exact zeros.
//!
//! Modulation. The LFO is per voice and restarts with the note; it modulates pitch (semitones),
//! filter cutoff (semitones) and amplitude (tremolo). The filter envelope moves the cutoff by
//! `Env Amount` semitones; key tracking adds `Key Track` semitones per semitone above middle C.
//!
//! Control rate. Filter coefficients, pitch, modulation and parameter glides update every
//! [`CONTROL`] frames, counted from the last reset (so block sizes and event positions do not
//! change the render); envelopes and oscillators run per sample. Level, tuning, cutoff,
//! resonance, depth and gain parameters glide over a few milliseconds, so turning a knob does not
//! zip; choices and envelope times apply at the next control update.
//!
//! Controllers. Controller 1 (mod wheel) adds up to [`WHEEL_VIBRATO`] semitones of LFO pitch depth;
//! controller 64 (sustain pedal, down from value 64) holds released notes until it comes up.
//! Both return to rest on a reset.
//!
//! With one unison voice and no oscillator 2, sub or noise, the output is the same signal on both
//! channels. The first patch of the original single-oscillator synth (ids 0 to 7) is still what
//! the default parameters play.

use std::f64::consts::TAU;

use super::common::{
    CONTROL, EnvCoefs, Envelope, FilterMode, MAX_STEP, Rng, Slot, Svf, SvfCoefs, db_to_gain,
    note_hz, pick_voice, poly_blamp, poly_blep,
};
use crate::device::{
    Device, DeviceInfo, Event, EventKind, ParamBank, ParamDescriptor, StateError, Unit,
};

/// Simultaneous voices.
pub use super::common::VOICES;

/// The `Device::kind` of the synth.
pub const KIND: &str = "builtin.synth";

/// Parameter id: oscillator 1 waveform (0 saw, 1 square, 2 sine, 3 triangle).
pub const PARAM_WAVEFORM: u32 = 0;
/// Parameter id: low-pass cutoff in hertz.
pub const PARAM_CUTOFF: u32 = 1;
/// Parameter id: filter resonance, 0 (none) to 1 (strong).
pub const PARAM_RESONANCE: u32 = 2;
/// Parameter id: amplitude envelope attack time in seconds.
pub const PARAM_ATTACK: u32 = 3;
/// Parameter id: amplitude envelope decay time in seconds.
pub const PARAM_DECAY: u32 = 4;
/// Parameter id: amplitude envelope sustain level, 0 to 1.
pub const PARAM_SUSTAIN: u32 = 5;
/// Parameter id: amplitude envelope release time in seconds.
pub const PARAM_RELEASE: u32 = 6;
/// Parameter id: output gain in decibels.
pub const PARAM_GAIN: u32 = 7;
/// Parameter id: oscillator 1 level, 0 to 1.
pub const PARAM_OSC1_LEVEL: u32 = 8;
/// Parameter id: oscillator 1 octave (choice index; 2 is the played octave).
pub const PARAM_OSC1_OCTAVE: u32 = 9;
/// Parameter id: oscillator 1 tuning in semitones.
pub const PARAM_OSC1_SEMITONE: u32 = 10;
/// Parameter id: oscillator 1 fine tuning in cents.
pub const PARAM_OSC1_FINE: u32 = 11;
/// Parameter id: oscillator 2 waveform.
pub const PARAM_OSC2_WAVEFORM: u32 = 12;
/// Parameter id: oscillator 2 level, 0 to 1.
pub const PARAM_OSC2_LEVEL: u32 = 13;
/// Parameter id: oscillator 2 octave (choice index; 2 is the played octave).
pub const PARAM_OSC2_OCTAVE: u32 = 14;
/// Parameter id: oscillator 2 tuning in semitones.
pub const PARAM_OSC2_SEMITONE: u32 = 15;
/// Parameter id: oscillator 2 fine tuning in cents.
pub const PARAM_OSC2_FINE: u32 = 16;
/// Parameter id: sub oscillator level, 0 to 1.
pub const PARAM_SUB_LEVEL: u32 = 17;
/// Parameter id: sub oscillator waveform (0 sine, 1 square).
pub const PARAM_SUB_WAVEFORM: u32 = 18;
/// Parameter id: sub oscillator octave (0 is one octave down, 1 two).
pub const PARAM_SUB_OCTAVE: u32 = 19;
/// Parameter id: noise level, 0 to 1.
pub const PARAM_NOISE_LEVEL: u32 = 20;
/// Parameter id: filter type (0 low-pass, 1 high-pass, 2 band-pass).
pub const PARAM_FILTER_TYPE: u32 = 21;
/// Parameter id: filter key tracking, 0 to 1 (1 follows the keyboard exactly).
pub const PARAM_KEY_TRACK: u32 = 22;
/// Parameter id: filter envelope amount in semitones of cutoff.
pub const PARAM_FILTER_ENV_AMOUNT: u32 = 23;
/// Parameter id: filter envelope attack time in seconds.
pub const PARAM_FILTER_ATTACK: u32 = 24;
/// Parameter id: filter envelope decay time in seconds.
pub const PARAM_FILTER_DECAY: u32 = 25;
/// Parameter id: filter envelope sustain level, 0 to 1.
pub const PARAM_FILTER_SUSTAIN: u32 = 26;
/// Parameter id: filter envelope release time in seconds.
pub const PARAM_FILTER_RELEASE: u32 = 27;
/// Parameter id: LFO shape (0 sine, 1 triangle, 2 square, 3 saw down, 4 random).
pub const PARAM_LFO_SHAPE: u32 = 28;
/// Parameter id: LFO rate in hertz.
pub const PARAM_LFO_RATE: u32 = 29;
/// Parameter id: LFO to pitch, in semitones at full swing.
pub const PARAM_LFO_PITCH: u32 = 30;
/// Parameter id: LFO to cutoff, in semitones at full swing.
pub const PARAM_LFO_CUTOFF: u32 = 31;
/// Parameter id: LFO to amplitude (tremolo depth), 0 to 1.
pub const PARAM_LFO_AMP: u32 = 32;
/// Parameter id: unison voices (choice index; the count is the index plus one).
pub const PARAM_UNISON_VOICES: u32 = 33;
/// Parameter id: unison detune in cents (the outermost copies).
pub const PARAM_UNISON_DETUNE: u32 = 34;
/// Parameter id: unison stereo spread, 0 (center) to 1 (hard left and right).
pub const PARAM_UNISON_SPREAD: u32 = 35;

/// Waveform choice labels, in index order.
pub const WAVEFORMS: &[&str] = &["Saw", "Square", "Sine", "Triangle"];
/// Octave choice labels; the index 2 is the played octave.
pub const OCTAVES: &[&str] = &["-2", "-1", "0", "+1", "+2"];
/// Sub waveform labels.
pub const SUB_WAVEFORMS: &[&str] = &["Sine", "Square"];
/// Sub octave labels.
pub const SUB_OCTAVES: &[&str] = &["-1 oct", "-2 oct"];
/// Filter type labels.
pub const FILTER_TYPES: &[&str] = &["Low-pass", "High-pass", "Band-pass"];
/// LFO shape labels.
pub const LFO_SHAPES: &[&str] = &["Sine", "Triangle", "Square", "Saw down", "Random"];
/// Unison choice labels (the number of voices).
pub const UNISON_COUNTS: &[&str] = &["1", "2", "3", "4", "5", "6", "7"];

/// Vibrato depth in semitones the mod wheel adds at full travel.
pub const WHEEL_VIBRATO: f64 = 1.0;

/// Most unison copies of an oscillator.
pub const UNISON_MAX: usize = 7;

const PARAMS: &[ParamDescriptor] = &[
    ParamDescriptor::choice(PARAM_WAVEFORM, "Waveform", WAVEFORMS, 0).group("Osc 1"),
    ParamDescriptor::linear(PARAM_OSC1_LEVEL, "Osc 1 Level", 0.0, 1.0, 1.0, Unit::None)
        .group("Osc 1")
        .smoothed(10.0),
    ParamDescriptor::choice(PARAM_OSC1_OCTAVE, "Osc 1 Octave", OCTAVES, 2).group("Osc 1"),
    ParamDescriptor::linear(
        PARAM_OSC1_SEMITONE,
        "Osc 1 Semitone",
        -12.0,
        12.0,
        0.0,
        Unit::Semitones,
    )
    .group("Osc 1")
    .smoothed(5.0),
    ParamDescriptor::linear(
        PARAM_OSC1_FINE,
        "Osc 1 Fine",
        -100.0,
        100.0,
        0.0,
        Unit::Cents,
    )
    .group("Osc 1")
    .smoothed(5.0),
    ParamDescriptor::choice(PARAM_OSC2_WAVEFORM, "Osc 2 Waveform", WAVEFORMS, 0).group("Osc 2"),
    ParamDescriptor::linear(PARAM_OSC2_LEVEL, "Osc 2 Level", 0.0, 1.0, 0.0, Unit::None)
        .group("Osc 2")
        .smoothed(10.0),
    ParamDescriptor::choice(PARAM_OSC2_OCTAVE, "Osc 2 Octave", OCTAVES, 2).group("Osc 2"),
    ParamDescriptor::linear(
        PARAM_OSC2_SEMITONE,
        "Osc 2 Semitone",
        -12.0,
        12.0,
        0.0,
        Unit::Semitones,
    )
    .group("Osc 2")
    .smoothed(5.0),
    ParamDescriptor::linear(
        PARAM_OSC2_FINE,
        "Osc 2 Fine",
        -100.0,
        100.0,
        0.0,
        Unit::Cents,
    )
    .group("Osc 2")
    .smoothed(5.0),
    ParamDescriptor::linear(PARAM_SUB_LEVEL, "Sub Level", 0.0, 1.0, 0.0, Unit::None)
        .group("Sub and noise")
        .smoothed(10.0),
    ParamDescriptor::choice(PARAM_SUB_WAVEFORM, "Sub Waveform", SUB_WAVEFORMS, 0)
        .group("Sub and noise"),
    ParamDescriptor::choice(PARAM_SUB_OCTAVE, "Sub Octave", SUB_OCTAVES, 0).group("Sub and noise"),
    ParamDescriptor::linear(PARAM_NOISE_LEVEL, "Noise Level", 0.0, 1.0, 0.0, Unit::None)
        .group("Sub and noise")
        .smoothed(10.0),
    ParamDescriptor::choice(PARAM_FILTER_TYPE, "Filter Type", FILTER_TYPES, 0).group("Filter"),
    ParamDescriptor::log(PARAM_CUTOFF, "Cutoff", 20.0, 20_000.0, 8_000.0, Unit::Hertz)
        .group("Filter")
        .smoothed(10.0),
    ParamDescriptor::linear(PARAM_RESONANCE, "Resonance", 0.0, 1.0, 0.1, Unit::None)
        .group("Filter")
        .smoothed(10.0),
    ParamDescriptor::linear(PARAM_KEY_TRACK, "Key Track", 0.0, 1.0, 0.0, Unit::None)
        .group("Filter")
        .smoothed(10.0),
    ParamDescriptor::linear(
        PARAM_FILTER_ENV_AMOUNT,
        "Filter Env Amount",
        -96.0,
        96.0,
        0.0,
        Unit::Semitones,
    )
    .group("Filter")
    .smoothed(10.0),
    ParamDescriptor::log(
        PARAM_FILTER_ATTACK,
        "Filter Attack",
        0.001,
        5.0,
        0.01,
        Unit::Seconds,
    )
    .group("Filter Envelope"),
    ParamDescriptor::log(
        PARAM_FILTER_DECAY,
        "Filter Decay",
        0.001,
        5.0,
        0.3,
        Unit::Seconds,
    )
    .group("Filter Envelope"),
    ParamDescriptor::linear(
        PARAM_FILTER_SUSTAIN,
        "Filter Sustain",
        0.0,
        1.0,
        0.5,
        Unit::None,
    )
    .group("Filter Envelope"),
    ParamDescriptor::log(
        PARAM_FILTER_RELEASE,
        "Filter Release",
        0.001,
        10.0,
        0.3,
        Unit::Seconds,
    )
    .group("Filter Envelope"),
    ParamDescriptor::log(PARAM_ATTACK, "Attack", 0.001, 5.0, 0.01, Unit::Seconds)
        .group("Amp Envelope"),
    ParamDescriptor::log(PARAM_DECAY, "Decay", 0.001, 5.0, 0.2, Unit::Seconds)
        .group("Amp Envelope"),
    ParamDescriptor::linear(PARAM_SUSTAIN, "Sustain", 0.0, 1.0, 0.7, Unit::None)
        .group("Amp Envelope"),
    ParamDescriptor::log(PARAM_RELEASE, "Release", 0.001, 10.0, 0.3, Unit::Seconds)
        .group("Amp Envelope"),
    ParamDescriptor::choice(PARAM_LFO_SHAPE, "LFO Shape", LFO_SHAPES, 0).group("LFO"),
    ParamDescriptor::log(PARAM_LFO_RATE, "LFO Rate", 0.05, 30.0, 5.0, Unit::Hertz)
        .group("LFO")
        .smoothed(20.0),
    ParamDescriptor::linear(
        PARAM_LFO_PITCH,
        "LFO to Pitch",
        0.0,
        12.0,
        0.0,
        Unit::Semitones,
    )
    .group("LFO")
    .smoothed(20.0),
    ParamDescriptor::linear(
        PARAM_LFO_CUTOFF,
        "LFO to Cutoff",
        0.0,
        48.0,
        0.0,
        Unit::Semitones,
    )
    .group("LFO")
    .smoothed(20.0),
    ParamDescriptor::linear(PARAM_LFO_AMP, "LFO to Amp", 0.0, 1.0, 0.0, Unit::None)
        .group("LFO")
        .smoothed(20.0),
    ParamDescriptor::choice(PARAM_UNISON_VOICES, "Unison Voices", UNISON_COUNTS, 0).group("Unison"),
    ParamDescriptor::linear(
        PARAM_UNISON_DETUNE,
        "Unison Detune",
        0.0,
        100.0,
        20.0,
        Unit::Cents,
    )
    .group("Unison")
    .smoothed(20.0),
    ParamDescriptor::linear(
        PARAM_UNISON_SPREAD,
        "Unison Spread",
        0.0,
        1.0,
        0.5,
        Unit::None,
    )
    .group("Unison")
    .smoothed(20.0),
    ParamDescriptor::linear(PARAM_GAIN, "Gain", -60.0, 6.0, -9.0, Unit::Decibels)
        .group("Output")
        .smoothed(10.0),
];

/// Static description and constructor of the synth.
pub const INFO: DeviceInfo =
    DeviceInfo::new(KIND, "Synth", true, PARAMS, || Box::new(Synth::new()));

/// One oscillator's settings for a control interval.
#[derive(Debug, Clone, Copy)]
struct OscFrame {
    wave: u8,
    level: f32,
    /// Tuning from the played note, in semitones.
    semis: f64,
}

/// Every parameter-derived value the voices use for one control interval.
#[derive(Debug, Clone, Copy)]
struct Frame {
    osc: [OscFrame; 2],
    sub_level: f32,
    sub_square: bool,
    sub_shift: f64,
    noise: f32,
    mode: FilterMode,
    cutoff: f64,
    q: f64,
    key_track: f64,
    env_amount: f64,
    lfo_shape: u8,
    lfo_inc: f64,
    lfo_pitch: f64,
    lfo_cutoff: f64,
    lfo_amp: f32,
    unison: usize,
    /// Per unison copy: tuning offset in semitones and left/right gains (level-normalised).
    detunes: [f64; UNISON_MAX],
    pans: [(f32, f32); UNISON_MAX],
    gain: f32,
    amp: EnvCoefs,
    fenv: EnvCoefs,
}

#[derive(Debug, Clone, Copy)]
struct Voice {
    active: bool,
    /// The note is held (no note-off yet).
    gate: bool,
    /// The note-off came while the sustain pedal was down; the release waits for the pedal.
    sustained: bool,
    key: u8,
    /// Note-on counter value, for stealing the oldest.
    age: u64,
    velocity: f32,
    phases: [[f64; UNISON_MAX]; 2],
    steps: [[f64; UNISON_MAX]; 2],
    sub_phase: f64,
    sub_step: f64,
    amp: Envelope,
    fenv: Envelope,
    filters: [Svf; 2],
    coefs: SvfCoefs,
    lfo_phase: f64,
    lfo_hold: f32,
    lfo_rng: Rng,
    /// Noise source, seeded per note so the voices' order of rendering does not matter.
    noise: Rng,
    tremolo: f32,
}

impl Voice {
    const fn idle() -> Self {
        Self {
            active: false,
            gate: false,
            sustained: false,
            key: 0,
            age: 0,
            velocity: 0.0,
            phases: [[0.0; UNISON_MAX]; 2],
            steps: [[0.0; UNISON_MAX]; 2],
            sub_phase: 0.0,
            sub_step: 0.0,
            amp: Envelope::idle(),
            fenv: Envelope::idle(),
            filters: [Svf::new(); 2],
            coefs: SvfCoefs::flat(),
            lfo_phase: 0.0,
            lfo_hold: 0.0,
            lfo_rng: Rng::new(1),
            noise: Rng::new(1),
            tremolo: 1.0,
        }
    }

    fn lfo_value(&self, shape: u8) -> f32 {
        let p = self.lfo_phase;
        match shape {
            0 => (p * TAU).sin() as f32,
            1 => {
                if p < 0.25 {
                    (4.0 * p) as f32
                } else if p < 0.75 {
                    (2.0 - 4.0 * p) as f32
                } else {
                    (4.0 * p - 4.0) as f32
                }
            }
            2 => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            3 => (1.0 - 2.0 * p) as f32,
            _ => self.lfo_hold,
        }
    }

    /// Advances the LFO over one control interval.
    fn advance_lfo(&mut self, f: &Frame) {
        self.lfo_phase += f.lfo_inc * CONTROL as f64;
        if self.lfo_phase >= 1.0 {
            self.lfo_phase -= self.lfo_phase.floor();
            self.lfo_hold = self.lfo_rng.next_bipolar();
        }
    }

    /// Recomputes pitch, filter and tremolo for the control interval that starts now.
    fn update_control(&mut self, f: &Frame, sample_rate: f64) {
        let lfo = self.lfo_value(f.lfo_shape);
        let key = f64::from(self.key);
        let pitch = f64::from(lfo) * f.lfo_pitch;
        for (steps, osc) in self.steps.iter_mut().zip(&f.osc) {
            for (step, detune) in steps.iter_mut().zip(&f.detunes).take(f.unison) {
                let freq = note_hz(key + osc.semis + pitch + detune);
                *step = (freq / sample_rate).min(MAX_STEP);
            }
        }
        self.sub_step = (note_hz(key + f.sub_shift + pitch) / sample_rate).min(MAX_STEP);
        let shift = f.key_track * (key - 60.0)
            + f.env_amount * f64::from(self.fenv.level)
            + f64::from(lfo) * f.lfo_cutoff;
        self.coefs = SvfCoefs::new(f.cutoff * (shift / 12.0).exp2(), f.q, sample_rate);
        self.tremolo = 1.0 - f.lfo_amp * (1.0 - lfo) * 0.5;
    }

    /// Adds `left.len()` frames of this voice into the output; frees the voice at the end of its
    /// release.
    fn render(&mut self, f: &Frame, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (mut sl, mut sr) = (0.0_f32, 0.0_f32);
            let oscs = self.phases.iter_mut().zip(&self.steps).zip(&f.osc);
            for ((phases, steps), osc) in oscs {
                let copies = phases.iter_mut().zip(steps).zip(&f.pans).take(f.unison);
                for ((phase, &step), &(pl, pr)) in copies {
                    *phase += step;
                    if *phase >= 1.0 {
                        *phase -= 1.0;
                    }
                    if osc.level > 0.0 {
                        let o = oscillator(osc.wave, *phase, step) * osc.level;
                        sl += o * pl;
                        sr += o * pr;
                    }
                }
            }
            self.sub_phase += self.sub_step;
            if self.sub_phase >= 1.0 {
                self.sub_phase -= 1.0;
            }
            let mut centre = 0.0_f32;
            if f.sub_level > 0.0 {
                let sub = if f.sub_square {
                    oscillator(1, self.sub_phase, self.sub_step)
                } else {
                    (self.sub_phase * TAU).sin() as f32
                };
                centre += sub * f.sub_level;
            }
            if f.noise > 0.0 {
                centre += self.noise.next_bipolar() * f.noise;
            }
            let fl = self.filters[0].tick(&self.coefs, sl + centre, f.mode);
            let fr = self.filters[1].tick(&self.coefs, sr + centre, f.mode);
            let level = self.amp.tick(&f.amp);
            self.fenv.tick(&f.fenv);
            let amp = level * self.velocity * self.tremolo;
            *l += fl * amp;
            *r += fr * amp;
            if self.amp.finished() {
                *self = Self::idle();
                return;
            }
        }
    }
}

/// The synth. Build it through [`INFO`] (or [`Synth::new`] and [`Device::prepare`]).
#[derive(Debug, Clone)]
pub struct Synth {
    sample_rate: f32,
    bank: ParamBank,
    frame: Frame,
    /// A parameter changed since `frame` was computed.
    dirty: bool,
    /// Mod wheel (controller 1), 0 to 1.
    wheel: f64,
    /// Sustain pedal (controller 64) is down.
    pedal: bool,
    /// Frames since the last control update point, 0 to `CONTROL - 1`.
    tick: usize,
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
        let bank = ParamBank::new(PARAMS);
        let mut synth = Self {
            sample_rate: 48_000.0,
            frame: compute_frame(&bank, 48_000.0, 0.0),
            bank,
            dirty: false,
            wheel: 0.0,
            pedal: false,
            tick: 0,
            voices: [Voice::idle(); VOICES],
            counter: 0,
        };
        synth.refresh_frame();
        synth
    }

    /// Number of voices currently producing sound.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    fn refresh_frame(&mut self) {
        self.frame = compute_frame(&self.bank, f64::from(self.sample_rate), self.wheel);
        self.dirty = false;
    }

    fn note_on(&mut self, key: u8, velocity: u8) {
        if self.dirty {
            self.refresh_frame();
        }
        self.counter += 1;
        let mut slots = [Slot {
            active: false,
            gate: false,
            level: 0.0,
            age: 0,
        }; VOICES];
        for (slot, voice) in slots.iter_mut().zip(&self.voices) {
            *slot = Slot {
                active: voice.active,
                gate: voice.gate,
                level: voice.amp.level,
                age: voice.age,
            };
        }
        let index = pick_voice(&slots);
        let frame = self.frame;
        let sample_rate = f64::from(self.sample_rate);
        let counter = self.counter;
        let voice = &mut self.voices[index];
        if !voice.active {
            *voice = Voice::idle();
            for phases in &mut voice.phases {
                for (j, phase) in phases.iter_mut().enumerate().take(frame.unison) {
                    *phase = j as f64 / frame.unison as f64;
                }
            }
        }
        voice.active = true;
        voice.gate = true;
        voice.sustained = false;
        voice.key = key;
        voice.age = counter;
        voice.amp.trigger();
        voice.fenv.trigger();
        voice.velocity = f32::from(velocity.clamp(1, 127)) / 127.0;
        voice.lfo_phase = 0.0;
        voice.lfo_rng = Rng::new((counter as u32).wrapping_mul(2_654_435_761) | 1);
        voice.lfo_hold = voice.lfo_rng.next_bipolar();
        voice.noise = Rng::new((counter as u32).wrapping_mul(40_503) ^ 0x5bd1_e995);
        voice.update_control(&frame, sample_rate);
    }

    fn note_off(&mut self, key: u8) {
        for voice in &mut self.voices {
            if voice.active && voice.gate && voice.key == key && !voice.sustained {
                if self.pedal {
                    voice.sustained = true;
                } else {
                    voice.gate = false;
                    voice.amp.release();
                    voice.fenv.release();
                }
            }
        }
    }

    fn control_change(&mut self, controller: u8, value: u8) {
        match controller {
            1 => {
                self.wheel = f64::from(value.min(127)) / 127.0;
                self.dirty = true;
            }
            64 => {
                let down = value >= 64;
                if self.pedal && !down {
                    for voice in self.voices.iter_mut().filter(|v| v.active && v.sustained) {
                        voice.sustained = false;
                        voice.gate = false;
                        voice.amp.release();
                        voice.fenv.release();
                    }
                }
                self.pedal = down;
            }
            _ => {}
        }
    }

    fn apply(&mut self, event: &Event) {
        match event.kind {
            EventKind::NoteOn { key, velocity } => self.note_on(key, velocity),
            EventKind::NoteOff { key } => self.note_off(key),
            EventKind::ControlChange { controller, value } => {
                self.control_change(controller, value);
            }
        }
    }

    /// Adds the voices' output into `left` and `right` (equal lengths), then applies the gain.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sample_rate = f64::from(self.sample_rate);
        let mut done = 0;
        while done < left.len() {
            if self.tick == 0 {
                self.bank.advance(CONTROL);
                self.refresh_frame();
                let frame = self.frame;
                for voice in self.voices.iter_mut().filter(|v| v.active) {
                    voice.advance_lfo(&frame);
                    voice.update_control(&frame, sample_rate);
                }
            }
            let n = (CONTROL - self.tick).min(left.len() - done);
            let frame = self.frame;
            let (l, r) = (&mut left[done..done + n], &mut right[done..done + n]);
            for voice in self.voices.iter_mut().filter(|v| v.active) {
                voice.render(&frame, l, r);
            }
            for sample in l.iter_mut().chain(r.iter_mut()) {
                *sample *= frame.gain;
            }
            self.tick = (self.tick + n) % CONTROL;
            done += n;
        }
    }
}

/// The values the voices use, from the bank's current (smoothed) parameters.
fn compute_frame(bank: &ParamBank, sample_rate: f64, wheel: f64) -> Frame {
    let v = |id| bank.value(id);
    let t = |id| bank.target(id);
    let osc = |wave, level, octave, semitone, fine| OscFrame {
        wave: t(wave) as u8,
        level: v(level) as f32,
        semis: (t(octave) - 2.0) * 12.0 + v(semitone) + v(fine) / 100.0,
    };
    let unison = (t(PARAM_UNISON_VOICES) as usize + 1).min(UNISON_MAX);
    let detune = v(PARAM_UNISON_DETUNE) / 100.0;
    let spread = v(PARAM_UNISON_SPREAD);
    let norm = (unison as f64).sqrt().recip();
    let mut detunes = [0.0; UNISON_MAX];
    let mut pans = [(1.0, 1.0); UNISON_MAX];
    if unison > 1 {
        for (j, (offset, pan)) in detunes
            .iter_mut()
            .zip(pans.iter_mut())
            .enumerate()
            .take(unison)
        {
            let position = 2.0 * j as f64 / (unison - 1) as f64 - 1.0;
            *offset = position * detune;
            let angle = (position * spread + 1.0) * std::f64::consts::FRAC_PI_4;
            let root_two = std::f64::consts::SQRT_2;
            *pan = (
                (angle.cos() * root_two * norm) as f32,
                (angle.sin() * root_two * norm) as f32,
            );
        }
    }
    let q = std::f64::consts::FRAC_1_SQRT_2 * (1.0 + v(PARAM_RESONANCE) * 15.0);
    Frame {
        osc: [
            osc(
                PARAM_WAVEFORM,
                PARAM_OSC1_LEVEL,
                PARAM_OSC1_OCTAVE,
                PARAM_OSC1_SEMITONE,
                PARAM_OSC1_FINE,
            ),
            osc(
                PARAM_OSC2_WAVEFORM,
                PARAM_OSC2_LEVEL,
                PARAM_OSC2_OCTAVE,
                PARAM_OSC2_SEMITONE,
                PARAM_OSC2_FINE,
            ),
        ],
        sub_level: v(PARAM_SUB_LEVEL) as f32,
        sub_square: t(PARAM_SUB_WAVEFORM) >= 1.0,
        sub_shift: -12.0 * (t(PARAM_SUB_OCTAVE) + 1.0),
        noise: v(PARAM_NOISE_LEVEL) as f32,
        mode: match t(PARAM_FILTER_TYPE) as u8 {
            0 => FilterMode::LowPass,
            1 => FilterMode::HighPass,
            _ => FilterMode::BandPass,
        },
        cutoff: v(PARAM_CUTOFF),
        q,
        key_track: v(PARAM_KEY_TRACK),
        env_amount: v(PARAM_FILTER_ENV_AMOUNT),
        lfo_shape: t(PARAM_LFO_SHAPE) as u8,
        lfo_inc: v(PARAM_LFO_RATE) / sample_rate,
        lfo_pitch: v(PARAM_LFO_PITCH) + wheel * WHEEL_VIBRATO,
        lfo_cutoff: v(PARAM_LFO_CUTOFF),
        lfo_amp: v(PARAM_LFO_AMP) as f32,
        unison,
        detunes,
        pans,
        gain: db_to_gain(v(PARAM_GAIN)),
        amp: EnvCoefs::new(
            t(PARAM_ATTACK),
            t(PARAM_DECAY),
            t(PARAM_SUSTAIN),
            t(PARAM_RELEASE),
            sample_rate,
        ),
        fenv: EnvCoefs::new(
            t(PARAM_FILTER_ATTACK),
            t(PARAM_FILTER_DECAY),
            t(PARAM_FILTER_SUSTAIN),
            t(PARAM_FILTER_RELEASE),
            sample_rate,
        ),
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
        3 => {
            // A peak at phase 0 and a trough at 0.5: slope changes of -8 and +8 per cycle.
            let naive = 4.0 * (phase - 0.5).abs() - 1.0;
            naive - 4.0 * step * poly_blamp(phase, step)
                + 4.0 * step * poly_blamp((phase + 0.5).fract(), step)
        }
        _ => (phase * TAU).sin(),
    };
    value as f32
}

impl Device for Synth {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = if sample_rate.is_finite() && sample_rate >= 8_000.0 {
            sample_rate
        } else {
            48_000.0
        };
        self.bank.prepare(self.sample_rate);
        self.reset();
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if self.bank.set(id, value) {
            self.dirty = true;
        }
    }

    fn reset(&mut self) {
        self.voices = [Voice::idle(); VOICES];
        self.tick = 0;
        self.wheel = 0.0;
        self.pedal = false;
        self.bank.snap();
        self.refresh_frame();
    }

    fn process(&mut self, audio: &mut [&mut [f32]; 2], events: &[Event]) {
        let [left, right] = audio;
        let frames = left.len().min(right.len());
        left[..frames].fill(0.0);
        right[..frames].fill(0.0);
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
            self.render(&mut left[position..end], &mut right[position..end]);
            position = end;
        }
    }

    fn save_state(&self) -> Vec<u8> {
        self.bank.save_state()
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), StateError> {
        self.bank.load_state(data)?;
        self.dirty = true;
        Ok(())
    }
}
