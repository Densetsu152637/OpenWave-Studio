//! `builtin.fm`: a four-operator phase-modulation synthesizer.
//!
//! Each of the four sine operators has a frequency ratio (a list of harmonic and sub-harmonic
//! ratios) with a fine detune in cents, an output level, a velocity sensitivity and its own ADSR
//! envelope. An *algorithm* picks which operators modulate which and which are heard (the
//! carriers); operator 4 can also modulate itself through the `Feedback` parameter (the mean of
//! its last two outputs, which keeps the loop stable). A modulator's output, scaled by its level
//! and envelope, shifts the phase of the operators it feeds by up to [`MOD_DEPTH`] cycles.
//!
//! Algorithms (`>` modulates; carriers are the operators with no `>` after them):
//!
//! | # | routing | carriers |
//! |---|---------|----------|
//! | 1 | 4>3>2>1 | 1 |
//! | 2 | (4+3)>2>1 | 1 |
//! | 3 | (3+2)>1, 4>3 | 1 |
//! | 4 | 2>1, 4>3 | 1, 3 |
//! | 5 | 4>1, 4>2, 4>3 | 1, 2, 3 |
//! | 6 | 3>1, 4>2 | 1, 2 |
//! | 7 | none (additive) | 1, 2, 3, 4 |
//! | 8 | 4>3 | 1, 2, 3 |
//!
//! Controllers: controller 1 (mod wheel) adds vibrato of up to [`WHEEL_VIBRATO`] semitones at
//! [`VIBRATO_HZ`]; controller 64 (sustain pedal) holds released notes until it comes up.
//!
//! Voices, stealing, envelopes, control-rate updates and parameter glides follow the subtractive
//! synth (see [`super::synth`]); the output is the same signal on both channels. Aliasing is that
//! of any FM at a high ratio: the operators are plain sines, with no oversampling.

use std::f64::consts::TAU;

use super::common::{CONTROL, EnvCoefs, Envelope, Slot, VOICES, db_to_gain, note_hz, pick_voice};
use crate::device::{
    Device, DeviceInfo, Event, EventKind, ParamBank, ParamDescriptor, StateError, Unit,
};

/// The `Device::kind` of the FM synth.
pub const KIND: &str = "builtin.fm";

/// Parameter id: algorithm (choice index, 0 to 7).
pub const PARAM_ALGORITHM: u32 = 0;
/// Parameter id: operator 4 feedback, 0 to 1.
pub const PARAM_FEEDBACK: u32 = 1;
/// Parameter id: output gain in decibels.
pub const PARAM_GAIN: u32 = 2;

/// Parameters per operator.
const PER_OP: u32 = 8;
/// Operators.
pub const OPERATORS: usize = 4;

/// Parameter id of operator `op` (0 to 3) field `field`.
pub const fn op_param(op: usize, field: OpField) -> u32 {
    3 + op as u32 * PER_OP + field as u32
}

/// The per-operator parameters, in id order within an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpField {
    /// Frequency ratio (choice index into [`RATIOS`]).
    Ratio = 0,
    /// Fine detune in cents.
    Fine = 1,
    /// Output level, 0 to 1.
    Level = 2,
    /// Velocity sensitivity, 0 to 1.
    VelocitySensitivity = 3,
    /// Envelope attack time in seconds.
    Attack = 4,
    /// Envelope decay time in seconds.
    Decay = 5,
    /// Envelope sustain level.
    Sustain = 6,
    /// Envelope release time in seconds.
    Release = 7,
}

/// The frequency ratios of the `Ratio` choice, in index order.
pub const RATIOS: [f64; 17] = [
    0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 12.0, 14.0, 16.0,
];
/// Labels of [`RATIOS`].
pub const RATIO_LABELS: &[&str] = &[
    "0.25", "0.5", "0.75", "1", "1.5", "2", "3", "4", "5", "6", "7", "8", "9", "10", "12", "14",
    "16",
];
/// Algorithm labels, in index order.
pub const ALGORITHMS: &[&str] = &[
    "4>3>2>1",
    "(4+3)>2>1",
    "(3+2)>1, 4>3",
    "2>1, 4>3",
    "4>1, 4>2, 4>3",
    "3>1, 4>2",
    "Additive",
    "4>3, 1, 2",
];

/// Vibrato depth in semitones at full mod wheel.
pub const WHEEL_VIBRATO: f64 = 0.5;
/// Vibrato rate in hertz.
pub const VIBRATO_HZ: f64 = 5.5;

/// Phase shift in cycles a modulator at full output applies.
pub const MOD_DEPTH: f64 = 2.0;

/// Per algorithm and operator: the operators (bit i is operator i + 1) that modulate it.
const MODULATORS: [[u8; OPERATORS]; 8] = [
    [0b0010, 0b0100, 0b1000, 0],
    [0b0010, 0b1100, 0, 0],
    [0b0110, 0, 0b1000, 0],
    [0b0010, 0, 0b1000, 0],
    [0b1000, 0b1000, 0b1000, 0],
    [0b0100, 0b1000, 0, 0],
    [0, 0, 0, 0],
    [0, 0, 0b1000, 0],
];
/// Per algorithm: the operators heard.
const CARRIERS: [u8; 8] = [
    0b0001, 0b0001, 0b0001, 0b0101, 0b0111, 0b0011, 0b1111, 0b0111,
];

const COUNT: usize = 3 + OPERATORS * PER_OP as usize;

const GROUPS: [&str; OPERATORS] = ["Op 1", "Op 2", "Op 3", "Op 4"];
const NAMES: [[&str; PER_OP as usize]; OPERATORS] = [
    [
        "Op 1 Ratio",
        "Op 1 Fine",
        "Op 1 Level",
        "Op 1 Velocity",
        "Op 1 Attack",
        "Op 1 Decay",
        "Op 1 Sustain",
        "Op 1 Release",
    ],
    [
        "Op 2 Ratio",
        "Op 2 Fine",
        "Op 2 Level",
        "Op 2 Velocity",
        "Op 2 Attack",
        "Op 2 Decay",
        "Op 2 Sustain",
        "Op 2 Release",
    ],
    [
        "Op 3 Ratio",
        "Op 3 Fine",
        "Op 3 Level",
        "Op 3 Velocity",
        "Op 3 Attack",
        "Op 3 Decay",
        "Op 3 Sustain",
        "Op 3 Release",
    ],
    [
        "Op 4 Ratio",
        "Op 4 Fine",
        "Op 4 Level",
        "Op 4 Velocity",
        "Op 4 Attack",
        "Op 4 Decay",
        "Op 4 Sustain",
        "Op 4 Release",
    ],
];
/// Default (ratio index, level, velocity sensitivity, attack, decay, sustain, release) per operator.
const DEFAULTS: [(usize, f64, f64, f64, f64, f64, f64); OPERATORS] = [
    (3, 1.0, 0.5, 0.005, 0.5, 0.6, 0.3),
    (5, 0.35, 0.5, 0.005, 0.3, 0.2, 0.3),
    (3, 0.0, 0.5, 0.005, 0.3, 0.5, 0.3),
    (3, 0.0, 0.5, 0.005, 0.3, 0.5, 0.3),
];

const fn operator_params(op: usize) -> [ParamDescriptor; PER_OP as usize] {
    let n = &NAMES[op];
    let (ratio, level, velocity, attack, decay, sustain, release) = DEFAULTS[op];
    let group = GROUPS[op];
    [
        ParamDescriptor::choice(op_param(op, OpField::Ratio), n[0], RATIO_LABELS, ratio)
            .group(group),
        ParamDescriptor::linear(
            op_param(op, OpField::Fine),
            n[1],
            -50.0,
            50.0,
            0.0,
            Unit::Cents,
        )
        .group(group)
        .smoothed(5.0),
        ParamDescriptor::linear(
            op_param(op, OpField::Level),
            n[2],
            0.0,
            1.0,
            level,
            Unit::None,
        )
        .group(group)
        .smoothed(10.0),
        ParamDescriptor::linear(
            op_param(op, OpField::VelocitySensitivity),
            n[3],
            0.0,
            1.0,
            velocity,
            Unit::None,
        )
        .group(group),
        ParamDescriptor::log(
            op_param(op, OpField::Attack),
            n[4],
            0.001,
            5.0,
            attack,
            Unit::Seconds,
        )
        .group(group),
        ParamDescriptor::log(
            op_param(op, OpField::Decay),
            n[5],
            0.001,
            5.0,
            decay,
            Unit::Seconds,
        )
        .group(group),
        ParamDescriptor::linear(
            op_param(op, OpField::Sustain),
            n[6],
            0.0,
            1.0,
            sustain,
            Unit::None,
        )
        .group(group),
        ParamDescriptor::log(
            op_param(op, OpField::Release),
            n[7],
            0.001,
            10.0,
            release,
            Unit::Seconds,
        )
        .group(group),
    ]
}

const fn build_params() -> [ParamDescriptor; COUNT] {
    let mut out = [ParamDescriptor::linear(0, "", 0.0, 1.0, 0.0, Unit::None); COUNT];
    out[0] = ParamDescriptor::choice(PARAM_ALGORITHM, "Algorithm", ALGORITHMS, 0).group("Global");
    out[1] = ParamDescriptor::linear(PARAM_FEEDBACK, "Feedback", 0.0, 1.0, 0.0, Unit::None)
        .group("Global")
        .smoothed(10.0);
    out[2] = ParamDescriptor::linear(PARAM_GAIN, "Gain", -60.0, 6.0, -12.0, Unit::Decibels)
        .group("Global")
        .smoothed(10.0);
    let mut op = 0;
    while op < OPERATORS {
        let params = operator_params(op);
        let mut k = 0;
        while k < PER_OP as usize {
            out[3 + op * PER_OP as usize + k] = params[k];
            k += 1;
        }
        op += 1;
    }
    out
}

const PARAM_LIST: [ParamDescriptor; COUNT] = build_params();

/// Static description and constructor of the FM synth.
pub const INFO: DeviceInfo = DeviceInfo::new(KIND, "FM Synth", true, &PARAM_LIST, || {
    Box::new(FmSynth::new())
});

/// Every parameter-derived value the voices use for one control interval.
#[derive(Debug, Clone, Copy)]
struct Frame {
    algorithm: usize,
    feedback: f32,
    gain: f32,
    /// Vibrato depth in semitones (the mod wheel).
    vibrato_depth: f64,
    /// Vibrato phase advance per control interval.
    vibrato_inc: f64,
    /// Frequency multiplier of each operator (ratio and fine tuning).
    ratio: [f64; OPERATORS],
    level: [f32; OPERATORS],
    velocity_sensitivity: [f32; OPERATORS],
    env: [EnvCoefs; OPERATORS],
}

#[derive(Debug, Clone, Copy)]
struct Voice {
    active: bool,
    gate: bool,
    /// The note-off came while the sustain pedal was down.
    sustained: bool,
    key: u8,
    age: u64,
    velocity: f32,
    /// Vibrato LFO phase, 0 to 1.
    vibrato: f64,
    phase: [f64; OPERATORS],
    step: [f64; OPERATORS],
    env: [Envelope; OPERATORS],
    /// The last two outputs of operator 4, for feedback.
    feedback: [f32; 2],
    /// Level of each operator including velocity sensitivity.
    gain: [f32; OPERATORS],
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
            vibrato: 0.0,
            phase: [0.0; OPERATORS],
            step: [0.0; OPERATORS],
            env: [Envelope::idle(); OPERATORS],
            feedback: [0.0; 2],
            gain: [0.0; OPERATORS],
        }
    }

    fn update_control(&mut self, f: &Frame, sample_rate: f64) {
        let swing = (self.vibrato * TAU).sin() * f.vibrato_depth;
        let base = note_hz(f64::from(self.key) + swing) / sample_rate;
        for (step, ratio) in self.step.iter_mut().zip(&f.ratio) {
            *step = (base * ratio).min(0.49);
        }
        let velocity = self.velocity;
        let gains = self.gain.iter_mut().zip(&f.level);
        for ((gain, level), sens) in gains.zip(&f.velocity_sensitivity) {
            *gain = level * (1.0 - sens + sens * velocity);
        }
    }

    /// The loudest carrier envelope level, for stealing.
    fn level(&self, carriers: u8) -> f32 {
        self.env
            .iter()
            .enumerate()
            .filter(|(op, _)| carriers >> op & 1 == 1)
            .fold(0.0, |m, (_, e)| m.max(e.level))
    }

    fn render(&mut self, f: &Frame, left: &mut [f32], right: &mut [f32]) {
        let mods = &MODULATORS[f.algorithm];
        let carriers = CARRIERS[f.algorithm];
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mut out = [0.0_f32; OPERATORS];
            for op in [3_usize, 2, 1, 0] {
                let mask = mods[op];
                let mut modulation = out
                    .iter()
                    .enumerate()
                    .filter(|(src, _)| mask >> src & 1 == 1)
                    .fold(0.0_f32, |sum, (_, o)| sum + o);
                if op == 3 {
                    modulation += f.feedback * 0.5 * (self.feedback[0] + self.feedback[1]);
                }
                self.phase[op] += self.step[op];
                if self.phase[op] >= 1.0 {
                    self.phase[op] -= 1.0;
                }
                let envelope = self.env[op].tick(&f.env[op]);
                let angle = (self.phase[op] + f64::from(modulation) * MOD_DEPTH) * TAU;
                out[op] = angle.sin() as f32 * envelope * self.gain[op];
            }
            self.feedback = [out[3], self.feedback[0]];
            let sample: f32 = out
                .iter()
                .enumerate()
                .filter(|(op, _)| carriers >> op & 1 == 1)
                .map(|(_, o)| o)
                .sum();
            *l += sample;
            *r += sample;
            let done = self
                .env
                .iter()
                .enumerate()
                .filter(|(op, _)| carriers >> op & 1 == 1)
                .all(|(_, e)| e.finished());
            if done {
                *self = Self::idle();
                return;
            }
        }
    }
}

/// The FM synth. Build it through [`INFO`] (or [`FmSynth::new`] and [`Device::prepare`]).
#[derive(Debug, Clone)]
pub struct FmSynth {
    sample_rate: f32,
    bank: ParamBank,
    frame: Frame,
    dirty: bool,
    wheel: f64,
    pedal: bool,
    tick: usize,
    voices: [Voice; VOICES],
    counter: u64,
}

impl Default for FmSynth {
    fn default() -> Self {
        Self::new()
    }
}

impl FmSynth {
    /// An FM synth with default parameters at 48 kHz; call [`Device::prepare`] for the real rate.
    pub fn new() -> Self {
        let bank = ParamBank::new(&PARAM_LIST);
        Self {
            sample_rate: 48_000.0,
            frame: compute_frame(&bank, 48_000.0, 0.0),
            bank,
            dirty: false,
            wheel: 0.0,
            pedal: false,
            tick: 0,
            voices: [Voice::idle(); VOICES],
            counter: 0,
        }
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
        let carriers = CARRIERS[self.frame.algorithm];
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
                level: voice.level(carriers),
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
        }
        voice.active = true;
        voice.gate = true;
        voice.sustained = false;
        voice.key = key;
        voice.age = counter;
        voice.velocity = f32::from(velocity.clamp(1, 127)) / 127.0;
        for env in &mut voice.env {
            env.trigger();
        }
        voice.update_control(&frame, sample_rate);
    }

    fn note_off(&mut self, key: u8) {
        for voice in &mut self.voices {
            if voice.active && voice.gate && voice.key == key && !voice.sustained {
                if self.pedal {
                    voice.sustained = true;
                } else {
                    voice.gate = false;
                    for env in &mut voice.env {
                        env.release();
                    }
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
                        for env in &mut voice.env {
                            env.release();
                        }
                    }
                }
                self.pedal = down;
            }
            _ => {}
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let sample_rate = f64::from(self.sample_rate);
        let mut done = 0;
        while done < left.len() {
            if self.tick == 0 {
                self.bank.advance(CONTROL);
                self.refresh_frame();
                let frame = self.frame;
                for voice in self.voices.iter_mut().filter(|v| v.active) {
                    voice.vibrato = (voice.vibrato + frame.vibrato_inc).fract();
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

fn compute_frame(bank: &ParamBank, sample_rate: f64, wheel: f64) -> Frame {
    let v = |id| bank.value(id);
    let t = |id| bank.target(id);
    let op = |op: usize, field| op_param(op, field);
    let each = |f: &dyn Fn(usize) -> f64| [f(0), f(1), f(2), f(3)];
    let ratio = each(&|o| {
        let index = (t(op(o, OpField::Ratio)) as usize).min(RATIOS.len() - 1);
        RATIOS[index] * (v(op(o, OpField::Fine)) / 1200.0).exp2()
    });
    let level = each(&|o| v(op(o, OpField::Level)));
    let sens = each(&|o| t(op(o, OpField::VelocitySensitivity)));
    let env = [0, 1, 2, 3].map(|o| {
        EnvCoefs::new(
            t(op(o, OpField::Attack)),
            t(op(o, OpField::Decay)),
            t(op(o, OpField::Sustain)),
            t(op(o, OpField::Release)),
            sample_rate,
        )
    });
    Frame {
        algorithm: (t(PARAM_ALGORITHM) as usize).min(ALGORITHMS.len() - 1),
        feedback: v(PARAM_FEEDBACK) as f32,
        gain: db_to_gain(v(PARAM_GAIN)),
        vibrato_depth: wheel * WHEEL_VIBRATO,
        vibrato_inc: VIBRATO_HZ * CONTROL as f64 / sample_rate,
        ratio,
        level: level.map(|x| x as f32),
        velocity_sensitivity: sens.map(|x| x as f32),
        env,
    }
}

impl Device for FmSynth {
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
                match event.kind {
                    EventKind::NoteOn { key, velocity } => self.note_on(key, velocity),
                    EventKind::NoteOff { key } => self.note_off(key),
                    EventKind::ControlChange { controller, value } => {
                        self.control_change(controller, value);
                    }
                }
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
