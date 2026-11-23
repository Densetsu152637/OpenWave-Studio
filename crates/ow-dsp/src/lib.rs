//! Built-in instruments and effects for OpenWave Studio.
//!
//! A [`Device`] processes stereo audio in place and takes note [`Event`]s with sample offsets;
//! its parameters are described by fixed [`ParamDescriptor`]s that a UI builds a panel from.
//! [`registry`] lists the built-in kinds. This crate depends on nothing, least of all on the
//! project model or the engine, so the devices can later be exported unchanged as plugins.
//!
//! Everything on the processing path is allocation-free and lock-free.

pub mod device;
pub mod instruments;
pub mod registry;

pub use device::{Device, DeviceInfo, Event, EventKind, ParamDescriptor, ParamKind, Scale, Unit};
pub use registry::{all, find};
