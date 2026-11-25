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
    ) -> c_int;
    fn ow_stretch_process(
        handle: *mut Raw,
        input: *const c_float,
        input_frames: usize,
        output: *mut c_float,
        output_frames: usize,
    ) -> c_int;
    fn ow_stretch_exact(
        handle: *mut Raw,
        input: *const c_float,
        input_frames: usize,
        output: *mut c_float,
        output_frames: usize,
    ) -> c_int;
}

/// Largest frame count the C++ library accepts (it counts in `int`).
const MAX_FRAMES: usize = c_int::MAX as usize;

/// One stretcher instance; all audio is interleaved. Not thread-safe: `&mut self` serialises
/// use, and the instance may move between threads between calls.
pub(super) struct Stretch {
    raw: *mut Raw,
    channels: usize,
}

// SAFETY: the instance is plain heap data with no thread affinity; it is only used through
// `&mut self`, so there is never concurrent access.
unsafe impl Send for Stretch {}

impl Stretch {
    /// `None` when the library could not allocate (or `channels` does not fit).
    pub(super) fn preset_default(channels: u32, sample_rate: u32) -> Option<Self> {
        let channels = channels.max(1);
        let count = c_int::try_from(channels).ok()?;
        // SAFETY: plain constructor; the result is checked below.
        let raw = unsafe { ow_stretch_create(count, sample_rate as c_float) };
        (!raw.is_null()).then_some(Self {
            raw,
            channels: channels as usize,
        })
    }

    /// Frame count of `len` interleaved samples, if it is whole and fits the library.
    fn frames(&self, len: usize) -> Option<usize> {
        (len.is_multiple_of(self.channels) && len / self.channels <= MAX_FRAMES)
            .then_some(len / self.channels)
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

    /// Feeds interleaved pre-roll `input` without producing output; false on failure.
    pub(super) fn seek(&mut self, input: &[f32], playback_rate: f64) -> bool {
        let Some(frames) = self.frames(input.len()) else {
            return false;
        };
        // SAFETY: the pointer and frame count describe exactly `input`.
        unsafe { ow_stretch_seek(self.raw, input.as_ptr(), frames, playback_rate) != 0 }
    }

    /// Consumes all of interleaved `input` and fills all of `output`; false on failure (the
    /// output is then unspecified).
    pub(super) fn process(&mut self, input: &[f32], output: &mut [f32]) -> bool {
        let (Some(in_frames), Some(out_frames)) =
            (self.frames(input.len()), self.frames(output.len()))
        else {
            return false;
        };
        // SAFETY: the pointers and frame counts describe exactly `input` and `output`.
        unsafe {
            ow_stretch_process(
                self.raw,
                input.as_ptr(),
                in_frames,
                output.as_mut_ptr(),
                out_frames,
            ) != 0
        }
    }

    /// Stretches a whole buffer with latency compensation; false when `output` is too short,
    /// a length is unusable or the library failed. Offline path, so the buffer shapes are
    /// asserted.
    pub(super) fn exact(&mut self, input: &[f32], output: &mut [f32]) -> bool {
        assert_eq!(input.len() % self.channels, 0, "input is not whole frames");
        assert_eq!(
            output.len() % self.channels,
            0,
            "output is not whole frames"
        );
        let (Some(in_frames), Some(out_frames)) =
            (self.frames(input.len()), self.frames(output.len()))
        else {
            return false;
        };
        // SAFETY: the pointers and frame counts describe exactly `input` and `output`.
        unsafe {
            ow_stretch_exact(
                self.raw,
                input.as_ptr(),
                in_frames,
                output.as_mut_ptr(),
                out_frames,
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
