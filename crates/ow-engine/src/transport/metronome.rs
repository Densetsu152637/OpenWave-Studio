//! The metronome click generator.

use std::f64::consts::TAU;

use crate::graph::{Node, ProcessContext};

/// Parameter index: click gain (linear, default 0.5).
pub const METRONOME_PARAM_GAIN: u32 = 0;
/// Click level of a downbeat before the gain.
pub const ACCENT_LEVEL: f32 = 1.0;
/// Click level of the other beats before the gain.
pub const BEAT_LEVEL: f32 = 0.6;
/// Downbeat click frequency in hertz.
pub const ACCENT_HZ: f64 = 1600.0;
/// Other beats' click frequency in hertz.
pub const BEAT_HZ: f64 = 1000.0;

/// Mono metronome: a short decaying sine burst per beat, accented on the downbeat.
///
/// Clicks start on the exact frame [`BlockTiming::for_each_click`](crate::BlockTiming::for_each_click)
/// reports, so a click's first sample sits at the sample its beat rounds to, independent of the
/// block size. The first sample of a click is `level * gain` (the burst starts at full
/// amplitude); the burst decays linearly to silence over [`click_len`](Self::click_len) samples.
/// During a count-in the clicks sound whether or not the metronome is on; during playback they
/// follow [`BlockTiming::metronome_enabled`](crate::BlockTiming::metronome_enabled). A click
/// still ringing when another starts is cut off by it.
#[derive(Debug, Clone)]
pub struct MetronomeNode {
    gain: f32,
    /// Samples played of the current click; `None` when silent.
    age: Option<usize>,
    accent: bool,
}

impl MetronomeNode {
    /// A metronome with gain 0.5.
    pub fn new() -> Self {
        Self {
            gain: 0.5,
            age: None,
            accent: false,
        }
    }

    /// Length of a click in samples (10 ms).
    pub fn click_len(sample_rate: u32) -> usize {
        (sample_rate as usize / 100).max(1)
    }

    fn render(&mut self, out: &mut [f32], from: usize, to: usize, sample_rate: u32) {
        let len = Self::click_len(sample_rate);
        let (level, hz) = if self.accent {
            (ACCENT_LEVEL, ACCENT_HZ)
        } else {
            (BEAT_LEVEL, BEAT_HZ)
        };
        let amplitude = level * self.gain;
        for slot in &mut out[from..to] {
            *slot = match self.age {
                Some(age) if age < len => {
                    let envelope = 1.0 - age as f32 / len as f32;
                    let wave = (TAU * hz * age as f64 / f64::from(sample_rate)).cos() as f32;
                    self.age = Some(age + 1);
                    amplitude * envelope * wave
                }
                _ => {
                    self.age = None;
                    0.0
                }
            };
        }
    }
}

impl Default for MetronomeNode {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for MetronomeNode {
    fn name(&self) -> &'static str {
        "metronome"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.frames;
        let sample_rate = ctx.sample_rate;
        let timing = ctx.timing;
        let out = ctx.outputs.channel_mut(0);
        let mut cursor = 0;
        timing.for_each_click(|click| {
            self.render(out, cursor, click.frame, sample_rate);
            cursor = click.frame;
            self.age = Some(0);
            self.accent = click.accent;
        });
        self.render(out, cursor, frames, sample_rate);
    }
    fn set_param(&mut self, param: u32, value: f32) {
        if param == METRONOME_PARAM_GAIN && value.is_finite() {
            self.gain = value;
        }
    }
}
