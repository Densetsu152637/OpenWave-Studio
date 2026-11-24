//! The [`Device`] trait: one instrument or effect, with the descriptors a UI builds its panel
//! from.
//!
//! A device processes stereo audio in place and receives note events with sample offsets. It
//! has no application types in its signature, so it can later be exported unchanged as a plugin.
//!
//! # Realtime contract
//!
//! [`Device::process`], [`Device::set_param`] and [`Device::reset`] run on the audio thread:
//! they must not allocate, lock, block or perform I/O. Everything is preallocated in
//! [`DeviceInfo::create`] or [`Device::prepare`], both called on a control thread.

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
            create,
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
