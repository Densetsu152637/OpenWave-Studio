//! One-shot audition of a buffer through the engine (the loop browser's preview).
//!
//! A [`Preview`] holds finished audio at the stream's sample rate. The control thread builds it
//! (decoding, stretching and converting happen there), sends it through a dedicated ring of the
//! control link and keeps a [`PreviewHandle`]; the audio thread mixes it into the device buffer
//! after the plan has rendered (so it is heard next to the project, on top of the master) and
//! hands it back through a retire ring when it has played out or been stopped, so the audio
//! thread never frees it. Mixing reads two atomics and never allocates, locks or blocks.
//!
//! A new preview replaces the one that is playing (the old one fades out with the stop fade
//! while it is retired). Stopping sets a flag the audio thread polls once per cycle and fades
//! out over [`STOP_FADE_FRAMES`], so nothing clicks.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ow_media::Pcm;

/// Length of the fade-in at the start, in frames.
pub const FADE_IN_FRAMES: usize = 64;
/// Length of the fade-out when a preview is stopped or replaced, in frames.
pub const STOP_FADE_FRAMES: usize = 480;

/// What the control thread keeps of a preview: whether it is still playing, how far it is, and
/// the stop switch.
#[derive(Debug)]
pub struct PreviewHandle {
    frames: u64,
    position: AtomicU64,
    stop: AtomicBool,
    done: AtomicBool,
}

impl PreviewHandle {
    /// Asks the audio thread to fade the preview out and end it.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// True once the preview has played to its end or been stopped (or was never delivered
    /// because the engine closed).
    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    /// Frames played so far.
    pub fn position(&self) -> u64 {
        self.position.load(Ordering::Relaxed)
    }

    /// Length in frames.
    pub fn frames(&self) -> u64 {
        self.frames
    }
}

/// The audio-thread half of a preview.
pub struct Preview {
    channels: Vec<Vec<f32>>,
    position: usize,
    /// Frames left of the stop fade once it has begun.
    fade_left: Option<usize>,
    handle: Arc<PreviewHandle>,
}

impl Preview {
    /// A preview of `pcm` (already at the stream's sample rate) and the handle that follows it.
    /// Mono audio plays on both sides; channels beyond the second are ignored.
    pub fn new(pcm: Pcm) -> (Self, Arc<PreviewHandle>) {
        let frames = pcm.frames();
        let handle = Arc::new(PreviewHandle {
            frames: frames as u64,
            position: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            done: AtomicBool::new(frames == 0),
        });
        let channels = pcm.channels.into_iter().take(2).collect();
        (
            Self {
                channels,
                position: 0,
                fade_left: None,
                handle: Arc::clone(&handle),
            },
            handle,
        )
    }

    /// True when the preview needs nothing more from the audio thread.
    pub fn is_ended(&self) -> bool {
        self.handle.done.load(Ordering::Relaxed)
    }

    /// Ends the preview now (the audio thread does this to a preview that is replaced).
    pub fn end(&mut self) {
        self.handle.done.store(true, Ordering::Relaxed);
    }

    /// Adds the next `out.len() / channels` frames to the interleaved device buffer `out`.
    pub fn mix(&mut self, out: &mut [f32], channels: usize) {
        if self.is_ended() || channels == 0 || self.channels.is_empty() {
            return;
        }
        if self.fade_left.is_none() && self.handle.stop.load(Ordering::Relaxed) {
            self.fade_left = Some(STOP_FADE_FRAMES);
        }
        let total = self.channels[0].len();
        for frame in out.chunks_mut(channels) {
            if self.position >= total {
                self.handle.done.store(true, Ordering::Relaxed);
                break;
            }
            let mut gain = 1.0_f32;
            if self.position < FADE_IN_FRAMES {
                gain = self.position as f32 / FADE_IN_FRAMES as f32;
            }
            if let Some(left) = self.fade_left.as_mut() {
                if *left == 0 {
                    self.handle.done.store(true, Ordering::Relaxed);
                    break;
                }
                gain *= *left as f32 / STOP_FADE_FRAMES as f32;
                *left -= 1;
            }
            let left_sample = self.channels[0][self.position];
            let right_sample = self
                .channels
                .get(1)
                .map_or(left_sample, |c| c[self.position]);
            frame[0] += left_sample * gain;
            if let Some(slot) = frame.get_mut(1) {
                *slot += right_sample * gain;
            }
            self.position += 1;
        }
        self.handle
            .position
            .store(self.position as u64, Ordering::Relaxed);
        if self.position >= total {
            self.handle.done.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(frames: usize) -> Pcm {
        Pcm {
            sample_rate: 48_000,
            channels: vec![vec![0.5; frames]],
        }
    }

    #[test]
    fn plays_to_the_end_on_both_sides_and_finishes() {
        let (mut preview, handle) = Preview::new(constant(300));
        let mut out = vec![0.25_f32; 2 * 200];
        preview.mix(&mut out, 2);
        assert!(!handle.finished());
        assert_eq!(out[2 * 150], 0.75);
        assert_eq!(out[2 * 150 + 1], 0.75);
        assert_eq!(out[0], 0.25, "the fade-in starts silent");
        let mut more = vec![0.0_f32; 2 * 200];
        preview.mix(&mut more, 2);
        assert!(handle.finished());
        assert_eq!(handle.position(), 300);
        assert_eq!(more[2 * 99 + 1], 0.5);
        assert_eq!(more[2 * 100], 0.0, "silent after the end");
    }

    #[test]
    fn stop_fades_out_and_finishes() {
        let (mut preview, handle) = Preview::new(constant(100_000));
        let mut out = vec![0.0_f32; 2 * 1000];
        preview.mix(&mut out, 2);
        handle.stop();
        let mut fade = vec![0.0_f32; 2 * 1000];
        preview.mix(&mut fade, 2);
        assert!(handle.finished());
        assert!(fade[0] > fade[2 * 400], "the level falls");
        assert_eq!(fade[2 * STOP_FADE_FRAMES], 0.0);
    }

    #[test]
    fn an_empty_buffer_is_finished_at_once() {
        let (mut preview, handle) = Preview::new(Pcm {
            sample_rate: 48_000,
            channels: vec![],
        });
        assert!(handle.finished());
        let mut out = vec![0.0_f32; 8];
        preview.mix(&mut out, 2);
        assert_eq!(out, vec![0.0; 8]);
    }
}
