//! A node that plays an [`ow_media::AudioSource`] at timeline positions.
//!
//! This is the stage 11 building block for audio clips; stage 13 builds one per clip (or per
//! track) in [`compile`](crate::compile) from the model. The node renders each span of the
//! block ([`BlockTiming::spans`](crate::BlockTiming::spans)), so loops, seeks and count-in are
//! sample-accurate and nothing plays while the transport is stopped.
//!
//! The source must already be at the plan's sample rate (import resamples to the project
//! rate. Fades are linear ramps (see [`ClipPlacement::with_fades`]); audio-clip compilation
//! stays in [`compile`](crate::compile) next to the other node builders. Placements that overlap are summed. A streamed source is read at each placement's
//! own position, so overlapping placements of one stream make it seek back and forth; give
//! every simultaneously playing clip its own source.

use ow_media::AudioSource;

use super::{Node, ProcessContext};

/// Where a region of the source plays on the timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipPlacement {
    /// Timeline sample at which the region starts.
    pub start: u64,
    /// Length in frames.
    pub length: u64,
    /// First source frame played.
    pub source_offset: u64,
    /// Linear gain.
    pub gain: f32,
    /// Length of the fade-in ramp in frames, from the start of the clip.
    pub fade_in: u64,
    /// Length of the fade-out ramp in frames, ending at the end of the clip.
    pub fade_out: u64,
}

impl ClipPlacement {
    /// The same placement with fades of `fade_in` and `fade_out` frames.
    ///
    /// Fades are linear gain ramps (0 to 1 over the fade-in, 1 to 0 over the fade-out) that
    /// multiply `gain`. They are clamped so that `fade_in + fade_out <= length`; the fade-in
    /// keeps its length and the fade-out gives way.
    #[must_use]
    pub fn with_fades(mut self, fade_in: u64, fade_out: u64) -> Self {
        self.fade_in = fade_in.min(self.length);
        self.fade_out = fade_out.min(self.length - self.fade_in);
        self
    }

    /// Linear fade envelope at `index` frames into the clip (1.0 outside the ramps).
    #[inline]
    fn envelope(&self, index: u64) -> f32 {
        let mut level = 1.0_f32;
        if index < self.fade_in {
            level = index as f32 / self.fade_in as f32;
        }
        let from_end = self.length.saturating_sub(index);
        if from_end <= self.fade_out {
            level = level.min(from_end as f32 / self.fade_out as f32);
        }
        level
    }
}

/// Plays an [`AudioSource`] through its placements; outputs the source's channels.
pub struct ClipSourceNode {
    source: Box<dyn AudioSource>,
    placements: Vec<ClipPlacement>,
    scratch: Vec<Vec<f32>>,
}

impl ClipSourceNode {
    /// A node playing `source` at `placements`.
    pub fn new(source: Box<dyn AudioSource>, placements: Vec<ClipPlacement>) -> Self {
        Self {
            source,
            placements,
            scratch: Vec::new(),
        }
    }
}

impl Node for ClipSourceNode {
    fn name(&self) -> &'static str {
        "clip-source"
    }

    fn input_channels(&self) -> usize {
        0
    }

    fn output_channels(&self) -> usize {
        self.source.channels()
    }

    fn prepare(&mut self, _sample_rate: u32, max_block: usize) {
        self.scratch = vec![vec![0.0; max_block]; self.source.channels()];
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let channels = self.source.channels();
        for channel in 0..channels {
            ctx.outputs.channel_mut(channel).fill(0.0);
        }
        let capacity = self.scratch.first().map_or(0, Vec::len);
        for span in ctx.timing.spans() {
            let span_start = span.start_sample();
            let span_end = span_start + span.frames() as u64;
            for placement in &self.placements {
                let from = placement.start.max(span_start);
                let to = placement
                    .start
                    .saturating_add(placement.length)
                    .min(span_end);
                if from >= to {
                    continue;
                }
                let count = ((to - from) as usize).min(capacity);
                let frame = span.frame() + (from - span_start) as usize;
                let position = placement.source_offset + (from - placement.start);
                self.source.read(position, &mut self.scratch, count);
                let first = from - placement.start;
                let faded = placement.fade_in > 0 || placement.fade_out > 0;
                for (channel, scratch) in self.scratch.iter().enumerate() {
                    let out = &mut ctx.outputs.channel_mut(channel)[frame..frame + count];
                    if faded {
                        for (k, (dst, src)) in out.iter_mut().zip(&scratch[..count]).enumerate() {
                            *dst += src * placement.gain * placement.envelope(first + k as u64);
                        }
                    } else {
                        for (dst, src) in out.iter_mut().zip(&scratch[..count]) {
                            *dst += src * placement.gain;
                        }
                    }
                }
            }
        }
    }
}
