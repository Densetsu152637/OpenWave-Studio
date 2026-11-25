//! Safe wrapper over the C ABI in `native/ow_stretch.h` (vendored Signalsmith Stretch).
//!
//! The declarations below are written by hand and must match that header.

#![allow(unsafe_code)]

use std::ffi::{c_double, c_float, c_int};

#[repr(C)]
struct Raw {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn ow_stretch_create(channels: c_int, sample_rate: c_float) -> *mut Raw;
    fn ow_stretch_destroy(handle: *mut Raw);
    fn ow_stretch_reset(handle: *mut Raw);
    fn ow_stretch_input_latency(handle: *const Raw) -> usize;
    fn ow_stretch_output_latency(handle: *const Raw) -> usize;
    fn ow_stretch_set_transpose_semitones(handle: *mut Raw, semitones: c_float);
    fn ow_stretch_seek(
        handle: *mut Raw,
        input: *const c_float,
        input_frames: usize,
        playback_rate: c_double,
    );
    fn ow_stretch_process(
        handle: *mut Raw,
        input: *const c_float,
        input_frames: usize,
        output: *mut c_float,
        output_frames: usize,
    );
    fn ow_stretch_exact(
        handle: *mut Raw,
        input: *const c_float,
        input_frames: usize,
        output: *mut c_float,
        output_frames: usize,
    ) -> c_int;
}

/// One stretcher instance; all audio is interleaved.
pub(super) struct Stretch {
    raw: *mut Raw,
    channels: usize,
}

// SAFETY: the instance is plain heap data with no thread affinity; it is only used through
// `&mut self`, so there is never concurrent access.
unsafe impl Send for Stretch {}

impl Stretch {
    pub(super) fn preset_default(channels: u32, sample_rate: u32) -> Self {
        let channels = channels.max(1);
        // SAFETY: plain constructor; the result is checked below.
        let raw = unsafe { ow_stretch_create(channels as c_int, sample_rate as c_float) };
        assert!(!raw.is_null(), "stretch instance allocation failed");
        Self {
            raw,
            channels: channels as usize,
        }
    }

    pub(super) fn reset(&mut self) {
        // SAFETY: `raw` is live until drop.
        unsafe { ow_stretch_reset(self.raw) }
    }

    pub(super) fn input_latency(&self) -> usize {
        // SAFETY: `raw` is live until drop.
        unsafe { ow_stretch_input_latency(self.raw) }
    }

    pub(super) fn output_latency(&self) -> usize {
        // SAFETY: `raw` is live until drop.
        unsafe { ow_stretch_output_latency(self.raw) }
    }

    pub(super) fn set_transpose_semitones(&mut self, semitones: f32) {
        // SAFETY: `raw` is live until drop.
        unsafe { ow_stretch_set_transpose_semitones(self.raw, semitones) }
    }

    /// Feeds interleaved pre-roll `input` without producing output.
    pub(super) fn seek(&mut self, input: &[f32], playback_rate: f64) {
        debug_assert_eq!(input.len() % self.channels, 0);
        // SAFETY: the pointer and frame count describe exactly `input`.
        unsafe {
            ow_stretch_seek(
                self.raw,
                input.as_ptr(),
                input.len() / self.channels,
                playback_rate,
            );
        }
    }

    /// Consumes all of interleaved `input` and fills all of `output`.
    pub(super) fn process(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len() % self.channels, 0);
        debug_assert_eq!(output.len() % self.channels, 0);
        // SAFETY: the pointers and frame counts describe exactly `input` and `output`.
        unsafe {
            ow_stretch_process(
                self.raw,
                input.as_ptr(),
                input.len() / self.channels,
                output.as_mut_ptr(),
                output.len() / self.channels,
            );
        }
    }

    /// Stretches a whole buffer with latency compensation; false when `output` is too short.
    pub(super) fn exact(&mut self, input: &[f32], output: &mut [f32]) -> bool {
        debug_assert_eq!(input.len() % self.channels, 0);
        debug_assert_eq!(output.len() % self.channels, 0);
        // SAFETY: the pointers and frame counts describe exactly `input` and `output`.
        unsafe {
            ow_stretch_exact(
                self.raw,
                input.as_ptr(),
                input.len() / self.channels,
                output.as_mut_ptr(),
                output.len() / self.channels,
            ) != 0
        }
    }
}

impl Drop for Stretch {
    fn drop(&mut self) {
        // SAFETY: `raw` came from `ow_stretch_create` and is destroyed exactly once.
        unsafe { ow_stretch_destroy(self.raw) }
    }
}
