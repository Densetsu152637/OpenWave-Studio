//! The device input inside the graph: the per-plan [`InputBus`] the audio thread fills with each
//! chunk's input, the [`InputMonitorNode`] that plays a track's routed channels through its strip
//! and the [`ArmedTracks`] flags that gate automatic monitoring.
//!
//! The bus follows the same rule as the plan's buffers: the audio thread writes it between
//! cycles (no node runs), nodes only read it during a cycle, and the DAG's publication of the
//! cycle orders the two.

// The bus is shared between the audio thread and the DSP workers behind `UnsafeCell`.
#![allow(unsafe_code)]

use std::cell::UnsafeCell;
use std::collections::BTreeMap;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use ow_model::TrackId;

use super::{Node, ProcessContext};

/// Seconds a monitor takes to fade in or out when its gate flips (no click on arming).
const MONITOR_FADE_SECONDS: f32 = 0.002;

/// De-interleaved copy of the device input for the block being rendered: `channels` channels of
/// up to `stride` frames.
pub struct InputBus {
    data: Box<[UnsafeCell<f32>]>,
    channels: usize,
    stride: usize,
}

// SAFETY: written only by the audio thread between cycles and read only by nodes during a cycle
// (see the module documentation), so no write overlaps a read.
unsafe impl Sync for InputBus {}

impl InputBus {
    /// A bus for `channels` input channels and blocks of up to `stride` frames; silent.
    pub(crate) fn new(channels: usize, stride: usize) -> Self {
        Self {
            data: (0..channels * stride)
                .map(|_| UnsafeCell::new(0.0))
                .collect(),
            channels,
            stride,
        }
    }

    /// Input channels the bus holds.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Copies `frames` frames of `interleaved` input (`in_channels` per frame) into the bus. A
    /// bus channel the device does not have is cleared. Allocation free.
    ///
    /// # Safety
    ///
    /// Audio thread only, between cycles: no node is reading the bus.
    pub(crate) unsafe fn write(&self, interleaved: &[f32], in_channels: usize, frames: usize) {
        debug_assert!(frames <= self.stride);
        let frames = frames.min(self.stride);
        for channel in 0..self.channels {
            // SAFETY: the range is within the allocation, and the caller rules out readers.
            let out = unsafe { self.channel_mut(channel, frames) };
            if channel < in_channels {
                for (slot, frame) in out.iter_mut().zip(interleaved.chunks_exact(in_channels)) {
                    *slot = frame[channel];
                }
            } else {
                out.fill(0.0);
            }
        }
    }

    /// Clears `frames` frames of every channel. Same contract as [`write`](Self::write).
    ///
    /// # Safety
    ///
    /// Audio thread only, between cycles.
    pub(crate) unsafe fn clear(&self, frames: usize) {
        for channel in 0..self.channels {
            // SAFETY: forwarded from this function's contract.
            unsafe { self.channel_mut(channel, frames.min(self.stride)) }.fill(0.0);
        }
    }

    #[allow(clippy::mut_from_ref)]
    unsafe fn channel_mut(&self, channel: usize, frames: usize) -> &mut [f32] {
        let start = channel * self.stride;
        let cells = &self.data[start..start + frames];
        // SAFETY: `UnsafeCell<f32>` has the layout of `f32`; exclusivity is the caller's.
        unsafe { slice::from_raw_parts_mut(UnsafeCell::raw_get(cells.as_ptr()), frames) }
    }

    /// Samples `0..frames` of input channel `channel`; `None` when the bus has no such channel.
    ///
    /// # Safety
    ///
    /// Nobody writes the bus while the slice is alive (nodes call this during a cycle).
    unsafe fn channel(&self, channel: usize, frames: usize) -> Option<&[f32]> {
        if channel >= self.channels || frames > self.stride {
            return None;
        }
        let start = channel * self.stride;
        let cells = &self.data[start..start + frames];
        // SAFETY: as above, and the caller rules out writers.
        Some(unsafe { slice::from_raw_parts(UnsafeCell::raw_get(cells.as_ptr()), frames) })
    }
}

impl std::fmt::Debug for InputBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputBus")
            .field("channels", &self.channels)
            .field("stride", &self.stride)
            .finish()
    }
}

/// Plays input channels through a track's strip: stereo out, from one channel (both sides) or
/// a pair. With a gate (automatic monitoring) the signal fades in and out as the gate flips;
/// without one it always plays.
pub struct InputMonitorNode {
    bus: Arc<InputBus>,
    first: usize,
    stereo: bool,
    gate: Option<Arc<AtomicBool>>,
    level: f32,
    step: f32,
}

impl InputMonitorNode {
    /// A monitor of channel `first` (and `first + 1` when `stereo`) of `bus`, open while `gate`
    /// is set (always when `None`).
    pub fn new(
        bus: Arc<InputBus>,
        first: usize,
        stereo: bool,
        gate: Option<Arc<AtomicBool>>,
    ) -> Self {
        Self {
            bus,
            first,
            stereo,
            gate,
            level: 0.0,
            step: 1.0,
        }
    }
}

impl Node for InputMonitorNode {
    fn name(&self) -> &'static str {
        "input-monitor"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        2
    }
    fn prepare(&mut self, sample_rate: u32, _max_block: usize) {
        self.step = 1.0 / (MONITOR_FADE_SECONDS * sample_rate.max(1) as f32).max(1.0);
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.frames;
        let target = match &self.gate {
            Some(gate) if !gate.load(Ordering::Relaxed) => 0.0,
            _ => 1.0,
        };
        // SAFETY: nodes run only during a cycle, when the audio thread does not write the bus.
        let left = unsafe { self.bus.channel(self.first, frames) };
        let right = if self.stereo {
            // SAFETY: as above.
            unsafe { self.bus.channel(self.first + 1, frames) }
        } else {
            left
        };
        let (Some(left), Some(right)) = (left, right) else {
            ctx.outputs.channel_mut(0).fill(0.0);
            ctx.outputs.channel_mut(1).fill(0.0);
            self.level = 0.0;
            return;
        };
        let start = self.level;
        let mut end = start;
        for (channel, source) in [(0, left), (1, right)] {
            let out = ctx.outputs.channel_mut(channel);
            let mut level = start;
            for (slot, sample) in out.iter_mut().zip(source) {
                level = if level < target {
                    (level + self.step).min(target)
                } else {
                    (level - self.step).max(target)
                };
                *slot = if sample.is_finite() {
                    sample * level
                } else {
                    0.0
                };
            }
            end = level;
        }
        self.level = end;
    }
}

/// Which tracks are armed for recording, as flags the audio thread can read: a monitor node in
/// automatic mode holds its track's flag and plays only while it is set. Shared by every clone;
/// changed on the control thread (arming) and read when compiling.
#[derive(Debug, Clone, Default)]
pub struct ArmedTracks {
    flags: Arc<Mutex<BTreeMap<TrackId, Arc<AtomicBool>>>>,
}

impl ArmedTracks {
    fn lock(&self) -> MutexGuard<'_, BTreeMap<TrackId, Arc<AtomicBool>>> {
        self.flags
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The flag of `track` (created cleared on first use).
    pub fn flag(&self, track: TrackId) -> Arc<AtomicBool> {
        Arc::clone(self.lock().entry(track).or_default())
    }

    /// Arms or disarms `track`.
    pub fn set(&self, track: TrackId, armed: bool) {
        self.flag(track).store(armed, Ordering::Relaxed);
    }

    /// Makes exactly `armed` the armed tracks.
    pub fn replace(&self, armed: &[TrackId]) {
        let mut flags = self.lock();
        for (track, flag) in flags.iter() {
            flag.store(armed.contains(track), Ordering::Relaxed);
        }
        for track in armed {
            flags
                .entry(*track)
                .or_default()
                .store(true, Ordering::Relaxed);
        }
    }

    /// Whether `track` is armed.
    pub fn is_armed(&self, track: TrackId) -> bool {
        self.lock()
            .get(&track)
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }
}
