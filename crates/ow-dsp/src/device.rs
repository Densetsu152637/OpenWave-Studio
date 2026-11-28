//! The [`Device`] trait: one instrument or effect, with the descriptors a UI builds its panel
//! from.
//!
//! A device processes stereo audio in place and receives note events with sample offsets. It
//! has no application types in its signature, so it can later be exported unchanged as a plugin.
//!
//! # Realtime contract
//!
//! [`Device::process`], [`Device::process_sidechain`], [`Device::set_param`] and
//! [`Device::reset`] run on the audio thread: they must not allocate, lock, block or perform I/O.
//! Everything is preallocated in [`DeviceInfo::create`] or [`Device::prepare`], both called on a
//! control thread, as are [`Device::save_state`] and [`Device::load_state`].
//!
//! # What a device declares
//!
//! * **Parameters** ([`ParamDescriptor`]): stable id, range, default, unit, scale, a choice list
//!   or a continuous range, a display `group` (a panel section, a plugin editor page) and a
//!   `smoothing_ms` (how long a change takes to glide; 0 steps at once). A UI is generated from
//!   these alone. [`ParamBank`] implements the smoothing for devices that want it.
//! * **Notes**: [`Event`]s with a frame offset inside the block.
//! * **State** ([`Device::save_state`] / [`Device::load_state`]): versioned bytes for anything the
//!   parameters do not capture; see [`StateError`] and the [`ParamBank`] snapshot format.
//! * **Latency** ([`Device::latency_frames`]): reported to delay compensation.
//! * **Sidechain** ([`DeviceInfo::sidechain`], [`Device::process_sidechain`]): a device that takes
//!   a second stereo signal (a compressor keyed from another strip) says so in its info and
//!   receives the signal in `process_sidechain`.

use std::fmt;

/// What a parameter's number means, for panels and for converting stored values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// A plain number (a ratio, a level 0 to 1, a choice index).
    None,
    /// Decibels.
    Decibels,
    /// Hertz.
    Hertz,
    /// Seconds.
    Seconds,
    /// A percentage, 0 to 100.
    Percent,
    /// Semitones (a pitch offset, or a modulation depth in pitch).
    Semitones,
    /// Cents, hundredths of a semitone.
    Cents,
}

/// How a control maps its travel to the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    /// Evenly between the minimum and the maximum.
    Linear,
    /// Evenly in the logarithm (frequencies, times). Needs a positive minimum.
    Log,
}

/// The shape of a parameter's value set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// Any value from `min` to `max`.
    Continuous,
    /// One of the named options; the value is the option's index (0, 1, 2, ...) and `min`/`max`
    /// span the indices.
    Choice(&'static [&'static str]),
}

/// A fixed description of one parameter. Parameter ids are stable: they are what saved projects
/// store (`Device::params`), so an id is never renumbered or reused.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamDescriptor {
    /// Stable id, unique within the device.
    pub id: u32,
    /// Name shown next to the control.
    pub name: &'static str,
    /// Smallest value.
    pub min: f64,
    /// Largest value.
    pub max: f64,
    /// Value used when a project does not set the parameter.
    pub default: f64,
    /// What the number means.
    pub unit: Unit,
    /// How a control maps its travel to the value.
    pub scale: Scale,
    /// Continuous or a choice list.
    pub kind: ParamKind,
    /// The panel section (and plugin-editor page) the parameter belongs to; empty for none.
    /// Sections appear in order of their first parameter.
    pub group: &'static str,
    /// Milliseconds a change takes to settle (a one-pole glide), 0 to apply at once. A choice is
    /// never smoothed whatever this says; [`ParamBank`] applies it.
    pub smoothing_ms: f32,
}

impl ParamDescriptor {
    /// A continuous linear parameter.
    pub const fn linear(
        id: u32,
        name: &'static str,
        min: f64,
        max: f64,
        default: f64,
        unit: Unit,
    ) -> Self {
        Self {
            id,
            name,
            min,
            max,
            default,
            unit,
            scale: Scale::Linear,
            kind: ParamKind::Continuous,
            group: "",
            smoothing_ms: 0.0,
        }
    }

    /// A continuous parameter that a control moves logarithmically (`min` must be positive).
    pub const fn log(
        id: u32,
        name: &'static str,
        min: f64,
        max: f64,
        default: f64,
        unit: Unit,
    ) -> Self {
        Self {
            scale: Scale::Log,
            ..Self::linear(id, name, min, max, default, unit)
        }
    }

    /// A parameter that picks one of `options` (at least one).
    pub const fn choice(
        id: u32,
        name: &'static str,
        options: &'static [&'static str],
        default: usize,
    ) -> Self {
        Self {
            id,
            name,
            min: 0.0,
            max: (options.len() - 1) as f64,
            default: default as f64,
            unit: Unit::None,
            scale: Scale::Linear,
            kind: ParamKind::Choice(options),
            group: "",
            smoothing_ms: 0.0,
        }
    }

    /// The same parameter in the panel section `group`.
    pub const fn group(self, group: &'static str) -> Self {
        Self { group, ..self }
    }

    /// The same parameter gliding over about `ms` milliseconds when it changes.
    pub const fn smoothed(self, ms: f32) -> Self {
        Self {
            smoothing_ms: ms,
            ..self
        }
    }

    /// `value` limited to the parameter's range (a choice is also rounded to an index). A value
    /// that is not a number becomes the default.
    pub fn clamp(&self, value: f64) -> f64 {
        if !value.is_finite() {
            return self.default;
        }
        let value = value.clamp(self.min, self.max);
        match self.kind {
            ParamKind::Continuous => value,
            ParamKind::Choice(_) => value.round(),
        }
    }
}

/// A note event for the block being processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// Frame of the block the event takes effect on (`0..frames`).
    pub offset: u32,
    /// What happens.
    pub kind: EventKind,
}

/// What an [`Event`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// Starts a note. `velocity` is 1 to 127.
    NoteOn {
        /// MIDI key, 0 to 127.
        key: u8,
        /// MIDI velocity.
        velocity: u8,
    },
    /// Releases every held note on `key`.
    NoteOff {
        /// MIDI key, 0 to 127.
        key: u8,
    },
    /// A MIDI control change (7-bit): the built-in synths follow controller 1 (mod wheel) and
    /// 64 (sustain pedal, down at 64 and above) and ignore the others.
    ControlChange {
        /// MIDI controller number, 0 to 127.
        controller: u8,
        /// Controller value, 0 to 127.
        value: u8,
    },
}

impl Event {
    /// A note-on at `offset`.
    pub const fn note_on(offset: u32, key: u8, velocity: u8) -> Self {
        Self {
            offset,
            kind: EventKind::NoteOn { key, velocity },
        }
    }

    /// A note-off at `offset`.
    pub const fn note_off(offset: u32, key: u8) -> Self {
        Self {
            offset,
            kind: EventKind::NoteOff { key },
        }
    }

    /// A control change at `offset`.
    pub const fn control_change(offset: u32, controller: u8, value: u8) -> Self {
        Self {
            offset,
            kind: EventKind::ControlChange { controller, value },
        }
    }
}

/// One instrument or effect instance.
///
/// Audio is stereo, non-interleaved, processed in place: on entry `audio[0]` and `audio[1]` hold
/// the device's input (an effect's signal; an instrument ignores it and overwrites it with its
/// output), on return they hold its output. Both slices have the same length, at most the
/// `max_block` given to [`prepare`](Self::prepare). `events` are sorted by offset; effects
/// ignore them.
pub trait Device: Send {
    /// Called on a control thread before the first `process` and whenever the sample rate or
    /// the largest block changes. Allocate here, never in `process`.
    fn prepare(&mut self, sample_rate: f32, max_block: usize);

    /// Sets parameter `id` to `value` (already inside the descriptor's range; an unknown id is
    /// ignored). Takes effect from the next sample.
    fn set_param(&mut self, id: u32, value: f64);

    /// Forgets all sounding notes and filter or delay memory (a stop, or a new playback pass).
    fn reset(&mut self);

    /// Renders `audio[0].len()` frames.
    fn process(&mut self, audio: &mut [&mut [f32]; 2], events: &[Event]);

    /// Frames by which the output lags the input (a lookahead effect's delay); 0 by default. The
    /// engine reads it when it compiles and delays every parallel path by the same amount (plugin
    /// delay compensation), so it must be correct right after [`prepare`](Self::prepare) and
    /// [`set_param`](Self::set_param); a change takes a recompile to be compensated.
    fn latency_frames(&self) -> u32 {
        0
    }

    /// Like [`process`](Self::process) for a device whose [`DeviceInfo::sidechain`] is set:
    /// `sidechain` is the external key signal, the same length as `audio`. The default ignores
    /// it. A host with no sidechain source connected calls `process` instead.
    fn process_sidechain(
        &mut self,
        audio: &mut [&mut [f32]; 2],
        sidechain: &[&[f32]; 2],
        events: &[Event],
    ) {
        let _ = sidechain;
        self.process(audio, events);
    }

    /// The device's saved state as versioned bytes (see [`StateError`]); empty when it keeps
    /// nothing beyond its parameters. Control thread only: it allocates.
    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }

    /// Restores what [`save_state`](Self::save_state) wrote, including parameter values; the
    /// host sets the project's own parameter values afterwards, so they win. Control thread only.
    /// Bytes from a newer version, or damaged bytes, are an error and leave the device as it was.
    fn load_state(&mut self, data: &[u8]) -> Result<(), StateError> {
        let _ = data;
        Ok(())
    }
}

/// Why [`Device::load_state`] refused some bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateError {
    /// The bytes end early or are not state at all.
    Malformed,
    /// Written by a newer version of the device than this one understands.
    UnsupportedVersion(u32),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => f.write_str("device state is damaged"),
            Self::UnsupportedVersion(v) => write!(f, "device state version {v} is not supported"),
        }
    }
}

impl std::error::Error for StateError {}

/// Version of the [`ParamBank`] snapshot format.
pub const STATE_VERSION: u32 = 1;

/// The values of a device's parameters with optional per-parameter smoothing.
///
/// Built once from a static descriptor list (allocates, control thread). [`set`](Self::set)
/// clamps and stores a target; [`advance`](Self::advance) moves every smoothed value toward its
/// target by a number of frames (closed form, so two half steps equal one whole step up to
/// rounding); [`value`](Self::value) is the current smoothed value and [`target`](Self::target)
/// the unsmoothed one. Until the first `advance` after [`snap`](Self::snap) (and so for the
/// values a host sets right after creating a device) a change applies at once, so a project's
/// values never glide in from the defaults. None of the per-sample methods allocate.
#[derive(Debug, Clone)]
pub struct ParamBank {
    params: &'static [ParamDescriptor],
    /// Descriptor slot of each id (`u16::MAX` for a gap).
    slot_of: Vec<u16>,
    target: Vec<f64>,
    current: Vec<f64>,
    /// Per-frame decay of the distance to the target; 0 for an unsmoothed parameter.
    decay: Vec<f64>,
    sample_rate: f64,
    /// Set by the first `advance`; cleared by `snap`.
    live: bool,
}

impl ParamBank {
    /// A bank of `params` at their defaults, for 48 kHz until [`prepare`](Self::prepare).
    pub fn new(params: &'static [ParamDescriptor]) -> Self {
        let max_id = params.iter().map(|p| p.id).max().unwrap_or(0) as usize;
        let mut slot_of = vec![u16::MAX; max_id + 1];
        for (slot, param) in params.iter().enumerate() {
            slot_of[param.id as usize] = slot as u16;
        }
        let values: Vec<f64> = params.iter().map(|p| p.default).collect();
        let mut bank = Self {
            params,
            slot_of,
            target: values.clone(),
            current: values,
            decay: vec![0.0; params.len()],
            sample_rate: 48_000.0,
            live: false,
        };
        bank.prepare(48_000.0);
        bank
    }

    /// Recomputes the glide rates for `sample_rate` and applies every target at once.
    pub fn prepare(&mut self, sample_rate: f32) {
        self.sample_rate = f64::from(sample_rate).max(1.0);
        for (decay, param) in self.decay.iter_mut().zip(self.params) {
            let tau = f64::from(param.smoothing_ms) / 1000.0;
            let smoothable = matches!(param.kind, ParamKind::Continuous);
            *decay = if tau > 0.0 && smoothable {
                (-1.0 / (tau * self.sample_rate)).exp()
            } else {
                0.0
            };
        }
        self.snap();
    }

    /// Applies every target at once; changes made before the next `advance` also apply at once.
    pub fn snap(&mut self) {
        self.current.copy_from_slice(&self.target);
        self.live = false;
    }

    /// Every parameter back to its default, applied at once.
    pub fn reset_to_defaults(&mut self) {
        for (value, param) in self.target.iter_mut().zip(self.params) {
            *value = param.default;
        }
        self.snap();
    }

    fn slot(&self, id: u32) -> Option<usize> {
        match self.slot_of.get(id as usize) {
            Some(&slot) if slot != u16::MAX => Some(usize::from(slot)),
            _ => None,
        }
    }

    /// Sets parameter `id` (clamped to its range; a value that is not a number becomes the
    /// default). False for an unknown id.
    pub fn set(&mut self, id: u32, value: f64) -> bool {
        let Some(slot) = self.slot(id) else {
            return false;
        };
        let value = self.params[slot].clamp(value);
        self.target[slot] = value;
        if !self.live || self.decay[slot] == 0.0 {
            self.current[slot] = value;
        }
        true
    }

    /// The smoothed value of parameter `id` (0 for an unknown id).
    pub fn value(&self, id: u32) -> f64 {
        self.slot(id).map_or(0.0, |slot| self.current[slot])
    }

    /// The value last set for parameter `id`, before smoothing (0 for an unknown id).
    pub fn target(&self, id: u32) -> f64 {
        self.slot(id).map_or(0.0, |slot| self.target[slot])
    }

    /// Moves every smoothed value `frames` frames toward its target.
    pub fn advance(&mut self, frames: usize) {
        self.live = true;
        let frames = frames.min(i32::MAX as usize) as i32;
        let slots = self.current.iter_mut().zip(&self.target).zip(&self.decay);
        for ((current, &target), &decay) in slots {
            if decay > 0.0 && *current != target {
                let next = target + (*current - target) * decay.powi(frames);
                *current = if (next - target).abs() < 1.0e-9 * (1.0 + target.abs()) {
                    target
                } else {
                    next
                };
            }
        }
    }

    /// Version, count, then (id, value) pairs, little endian; the targets, not the glide
    /// positions. Allocates.
    pub fn save_state(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.params.len() * 12);
        out.extend_from_slice(&STATE_VERSION.to_le_bytes());
        out.extend_from_slice(&(self.params.len() as u32).to_le_bytes());
        for (param, value) in self.params.iter().zip(&self.target) {
            out.extend_from_slice(&param.id.to_le_bytes());
            out.extend_from_slice(&value.to_le_bytes());
        }
        out
    }

    /// Reads a [`save_state`](Self::save_state) snapshot and applies it at once. Unknown ids are
    /// skipped (a snapshot from a newer device); nothing changes on error.
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), StateError> {
        let word = |at: usize| -> Option<[u8; 4]> { data.get(at..at + 4)?.try_into().ok() };
        let version = u32::from_le_bytes(word(0).ok_or(StateError::Malformed)?);
        if version == 0 || version > STATE_VERSION {
            return Err(StateError::UnsupportedVersion(version));
        }
        let count = u32::from_le_bytes(word(4).ok_or(StateError::Malformed)?) as usize;
        if data.len() != 8 + count.saturating_mul(12) {
            return Err(StateError::Malformed);
        }
        let entries: Vec<(u32, f64)> = (0..count)
            .map(|i| {
                let entry = &data[8 + i * 12..8 + (i + 1) * 12];
                let id = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]);
                let mut bits = [0_u8; 8];
                bits.copy_from_slice(&entry[4..]);
                (id, f64::from_le_bytes(bits))
            })
            .collect();
        for (id, value) in entries {
            self.set(id, value);
        }
        self.snap();
        Ok(())
    }
}

/// Static facts about a kind of device, plus its constructor.
#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    /// The `Device::kind` string stored in projects, for example `builtin.synth`.
    pub kind: &'static str,
    /// Name shown in menus and panels.
    pub name: &'static str,
    /// True for a device that makes sound from note events (a track's instrument); false for an
    /// effect that processes the track's audio.
    pub instrument: bool,
    /// The parameters, in display order.
    pub params: &'static [ParamDescriptor],
    /// True for a device that takes a sidechain (key) input.
    pub sidechain: bool,
    create: fn() -> Box<dyn Device>,
}

impl DeviceInfo {
    /// Describes a device kind; `create` builds an unprepared instance.
    pub const fn new(
        kind: &'static str,
        name: &'static str,
        instrument: bool,
        params: &'static [ParamDescriptor],
        create: fn() -> Box<dyn Device>,
    ) -> Self {
        Self {
            kind,
            name,
            instrument,
            params,
            sidechain: false,
            create,
        }
    }

    /// The same info for a device that takes a sidechain input.
    pub const fn with_sidechain(self) -> Self {
        Self {
            sidechain: true,
            ..self
        }
    }

    /// A new instance with every parameter at its default, prepared for `sample_rate` and
    /// blocks of up to `max_block` frames. Allocates: call it on a control thread.
    pub fn create(&self, sample_rate: f32, max_block: usize) -> Box<dyn Device> {
        let mut device = (self.create)();
        for param in self.params {
            device.set_param(param.id, param.default);
        }
        device.prepare(sample_rate, max_block);
        device
    }

    /// The descriptor of parameter `id`.
    pub fn param(&self, id: u32) -> Option<&'static ParamDescriptor> {
        self.params.iter().find(|p| p.id == id)
    }
}
