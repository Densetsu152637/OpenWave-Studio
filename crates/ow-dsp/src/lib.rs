//! Built-in instruments and effects for OpenWave Studio.
//!
//! A [`Device`] processes stereo audio in place and takes note [`Event`]s with sample offsets;
//! its parameters are described by fixed [`ParamDescriptor`]s that a UI builds a panel from.
//! [`registry`] lists the built-in kinds. This crate depends on no OpenWave crate, least of all
//! on the project model or the engine, so the devices can later be exported unchanged as
//! plugins.
//!
//! Everything on the processing path is allocation-free and lock-free. [`stretch`] (time-stretch
//! and pitch-shift) and [`analysis`] (tempo, beat and key detection) are tools for the control
//! thread, except [`stretch::Stretcher`], which is made for the audio thread.

pub mod analysis;
pub mod device;
pub mod effects;
pub mod instruments;
pub mod registry;
pub mod stretch;

pub use device::{
    Device, DeviceInfo, Event, EventKind, ParamBank, ParamDescriptor, ParamKind, Scale, StateError,
    Unit,
};
pub use registry::{all, find};
