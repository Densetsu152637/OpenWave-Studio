//! Time-stretch and pitch-shift on top of Signalsmith Stretch (MIT; vendored in
//! `vendor/signalsmith-stretch`, compiled by `build.rs` with `cc` behind a small hand-declared
//! C ABI, so a C++ compiler is needed but libclang is not).
//!
//! * [`stretch_to_length`] renders a whole buffer offline to an exact frame count, for cached
//!   warped loops and in-tempo previews.
//! * [`Stretcher`] is the streaming form for the audio thread: everything it needs is allocated
//!   in [`Stretcher::new`] and warmed up there, so [`Stretcher::process`] allocates nothing on
//!   the Rust side.
//!
//! Ratios in this module are always `output duration / input duration`: 2.0 plays at half speed
//! (twice as long), 0.5 at double speed. Pitch is independent of tempo and given in semitones.

mod native;

use native::Stretch;

/// Largest pitch shift accepted in either direction, in semitones.
pub const MAX_PITCH_SEMITONES: f32 = 24.0;
/// Smallest ratio (output over input) a [`Stretcher`] accepts: four times faster.
pub const MIN_RATIO: f64 = 0.25;
/// Largest ratio (output over input) a [`Stretcher`] accepts: four times slower.
pub const MAX_RATIO: f64 = 4.0;

/// Writes `frames` frames of `channels`, starting at frame `start`, as `count` interleaved
/// channels into `out`; missing channels and frames are silent.
fn interleave(channels: &[&[f32]], count: usize, start: usize, frames: usize, out: &mut [f32]) {
    for c in 0..count {
        let source = channels.get(c).copied().unwrap_or(&[]);
        for i in 0..frames {
            let sample = source.get(start + i).copied().unwrap_or(0.0);
            // NaN and infinity would poison the algorithm's state; they read as silence.
            out[i * count + c] = if sample.is_finite() { sample } else { 0.0 };
        }
    }
}

fn set_pitch(stretch: &mut Stretch, semitones: f32) {
    let semitones = if semitones.is_finite() {
        semitones.clamp(-MAX_PITCH_SEMITONES, MAX_PITCH_SEMITONES)
    } else {
        0.0
    };
    stretch.set_transpose_semitones(semitones);
}

/// Stretches `input` (planar; channel `c` is `input[c]`, missing channels are silent, longer
/// ones are cut to the first channel's length) to exactly `target_frames` frames per channel,
/// shifting the pitch by `pitch_semitones` (clamped to +-24).
///
/// The result has `channels` vectors of `target_frames` samples each. The first frame of the
/// output lines up with the first frame of the input (the algorithm's latency is compensated),
/// and the tail is whatever the algorithm produces for the end of the input. Targets too short
/// for the algorithm's latency are rendered with zero padding on both sides, in proportion, and
/// cut back, so the length is exact for every target. Silence in, or no frames to produce, gives
/// silence.
///
/// Non-finite input samples (NaN, infinities) are treated as silence and the output is always
/// finite. If the algorithm cannot run (out of memory, or more than `i32::MAX` frames), the
/// result is silence of the exact length rather than an error or a panic, so a caller that
/// must tell the cases apart should check the output for silence. Allocates; control thread
/// only.
pub fn stretch_to_length(
    input: &[&[f32]],
    channels: usize,
    sample_rate: u32,
    target_frames: usize,
    pitch_semitones: f32,
) -> Vec<Vec<f32>> {
    let mut result = vec![vec![0.0_f32; target_frames]; channels];
    let in_frames = input.first().map_or(0, |c| c.len());
    if channels == 0 || target_frames == 0 || in_frames == 0 || sample_rate == 0 {
        return result;
    }
    let Some(mut stretch) = Stretch::preset_default(channels as u32, sample_rate) else {
        return result;
    };
    set_pitch(&mut stretch, pitch_semitones);
    // `exact` refuses outputs shorter than twice the output latency.
    let minimum = stretch.output_latency() * 2 + 64;
    let (mut in_total, mut out_total) = (in_frames, target_frames);
    if target_frames < minimum {
        let ratio = target_frames as f64 / in_frames as f64;
        out_total = minimum;
        in_total = in_frames + (((minimum - target_frames) as f64 / ratio).ceil() as usize);
    }
    let limit = i32::MAX as usize / channels;
    if in_total > limit || out_total > limit {
        return result;
    }
    let mut interleaved_in = vec![0.0_f32; in_total * channels];
    interleave(input, channels, 0, in_frames, &mut interleaved_in);
    let mut interleaved_out = vec![0.0_f32; out_total * channels];
    if !stretch.exact(&interleaved_in, &mut interleaved_out) {
        return result;
    }
    for (c, channel) in result.iter_mut().enumerate() {
        for (i, sample) in channel.iter_mut().enumerate() {
            let value = interleaved_out[i * channels + c];
            *sample = if value.is_finite() { value } else { 0.0 };
        }
    }
    result
}

/// Why a [`Stretcher`] could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StretchError {
    /// The stretch library could not allocate its state.
    Unavailable,
}

impl std::fmt::Display for StretchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "the time-stretch engine could not be created"),
        }
    }
}

impl std::error::Error for StretchError {}

/// A streaming time-stretcher and pitch-shifter for the audio thread.
///
/// Each [`Stretcher::process`] call fills one output block; the ratio can change between calls.
/// Latency: the first output frames trail the input by [`Stretcher::latency_frames`]; call
/// [`Stretcher::seek`] with the audio just before the start position to prime the algorithm
/// instead of hearing its ramp-up.
pub struct Stretcher {
    stretch: Stretch,
    channels: usize,
    max_block: usize,
    max_input: usize,
    carry: f64,
    input_buffer: Vec<f32>,
    output_buffer: Vec<f32>,
}

impl Stretcher {
    /// Prepares a stretcher for `channels` channels and output blocks of up to `max_block`
    /// frames (longer blocks are processed in several steps). Allocates and warms up the
    /// algorithm's internal buffers. Fails only when the library cannot allocate.
    pub fn new(channels: usize, sample_rate: u32, max_block: usize) -> Result<Self, StretchError> {
        let channels = channels.max(1);
        let max_block = max_block.max(1);
        let max_input = (max_block as f64 / MIN_RATIO).ceil() as usize + 2;
        let mut stretcher = Self {
            stretch: Stretch::preset_default(channels as u32, sample_rate.max(1))
                .ok_or(StretchError::Unavailable)?,
            channels,
            max_block,
            max_input,
            carry: 0.0,
            input_buffer: vec![0.0; max_input * channels],
            output_buffer: vec![0.0; max_block * channels],
        };
        stretcher.warm_up();
        Ok(stretcher)
    }

    /// Runs silence through the algorithm at both ratio extremes so the library's scratch
    /// vectors reach their final size, then resets.
    fn warm_up(&mut self) {
        let block = self.max_block;
        let seek_len = self.max_input;
        self.input_buffer[..seek_len * self.channels].fill(0.0);
        self.stretch
            .seek(&self.input_buffer[..seek_len * self.channels], 1.0);
        for ratio in [MIN_RATIO, MAX_RATIO, 1.0] {
            let wanted = ((block as f64 / ratio).round() as usize).clamp(1, self.max_input);
            self.stretch.process(
                &self.input_buffer[..wanted * self.channels],
                &mut self.output_buffer[..block * self.channels],
            );
        }
        self.stretch.reset();
    }

    /// Sets the pitch shift in semitones (clamped to +-24), independent of the ratio.
    pub fn set_pitch_semitones(&mut self, semitones: f32) {
        set_pitch(&mut self.stretch, semitones);
    }

    /// Frames the output trails the input by: the algorithm's output latency.
    pub fn latency_frames(&self) -> usize {
        self.stretch.output_latency()
    }

    /// Frames of input the algorithm wants before a start position, for [`Stretcher::seek`].
    pub fn input_latency_frames(&self) -> usize {
        self.stretch.input_latency()
    }

    /// Forgets all internal state and any fractional input carried between blocks.
    pub fn reset(&mut self) {
        self.stretch.reset();
        self.carry = 0.0;
    }

    /// Primes the algorithm with `input`, the audio just before the position playback starts
    /// at (at most the last `max_block / MIN_RATIO` frames are used), without producing output.
    pub fn seek(&mut self, input: &[&[f32]], ratio: f64) {
        let available = input.first().map_or(0, |c| c.len());
        let take = available.min(self.max_input);
        let from = available - take;
        interleave(input, self.channels, from, take, &mut self.input_buffer);
        self.stretch.seek(
            &self.input_buffer[..take * self.channels],
            1.0 / clamp_ratio(ratio),
        );
        self.carry = 0.0;
    }

    /// Input frames [`Stretcher::process`] will consume for `output_frames` frames of output at
    /// `ratio`, rounded up (the real count is that or one less, depending on carried fractions).
    pub fn input_frames_for(&self, output_frames: usize, ratio: f64) -> usize {
        (output_frames as f64 / clamp_ratio(ratio)).ceil() as usize
    }

    /// Fills `output` (planar, as many channels as given to [`Stretcher::new`]) from `input`
    /// (planar, positioned at the next unread frame) at `ratio` (output over input duration,
    /// clamped to [`MIN_RATIO`]..=[`MAX_RATIO`]). Returns the number of input frames consumed;
    /// the caller advances its read position by that much. Input that runs out reads as
    /// silence, and so does a block the library failed to render. Non-finite input reads as
    /// silence and the output is always finite. No allocation, lock or I/O on the Rust side.
    pub fn process(&mut self, input: &[&[f32]], output: &mut [&mut [f32]], ratio: f64) -> usize {
        let ratio = clamp_ratio(ratio);
        let out_frames = output.iter().map(|c| c.len()).min().unwrap_or(0);
        let mut consumed = 0;
        let mut done = 0;
        while done < out_frames {
            let chunk = (out_frames - done).min(self.max_block);
            let wanted = chunk as f64 / ratio + self.carry;
            let take = (wanted.floor() as usize).min(self.max_input);
            self.carry = wanted - take as f64;
            // Gather `take` input frames starting at `consumed`.
            for c in 0..self.channels {
                let source = input.get(c).copied().unwrap_or(&[]);
                for i in 0..take {
                    self.input_buffer[i * self.channels + c] =
                        source.get(consumed + i).copied().unwrap_or(0.0);
                }
            }
            let ran = self.stretch.process(
                &self.input_buffer[..take * self.channels],
                &mut self.output_buffer[..chunk * self.channels],
            );
            for (c, channel) in output.iter_mut().enumerate().take(self.channels) {
                for i in 0..chunk {
                    let sample = self.output_buffer[i * self.channels + c];
                    // A failed run or a non-finite value is silence, never noise.
                    channel[done + i] = if ran && sample.is_finite() {
                        sample
                    } else {
                        0.0
                    };
                }
            }
            consumed += take;
            done += chunk;
        }
        consumed
    }
}

fn clamp_ratio(ratio: f64) -> f64 {
    if ratio.is_finite() {
        ratio.clamp(MIN_RATIO, MAX_RATIO)
    } else {
        1.0
    }
}
