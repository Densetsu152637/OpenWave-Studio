//! Offline export: renders a project's mix without touching the realtime engine.
//!
//! [`render_project`] compiles a plan of its own for the project's sample rate (media comes
//! from the same [`CompileContext`] cache the live engine uses, at that rate), plays the
//! transport from 0 on an [`OfflineRenderer`] and returns the interleaved stereo mix. The
//! caller decides the length; [`export_frames`] gives the usual one, the end of the last clip
//! plus one second for effects tails.
//!
//! The plan's output latency (a lookahead limiter on the master, say) is compensated: the render
//! runs that many frames longer and drops the first ones, so the result is aligned with the
//! timeline and has exactly the requested length. [`render_strip`] renders one strip after its
//! fader instead of the master, for analysis.

use std::fmt;

use ow_model::{MixerRef, Project};

use crate::compile::{CompileContext, CompileError, compile_project_tap};
use crate::{MAX_BLOCK, Message, OfflineRenderer, Timeline, TransportCommand};

/// Longest export, in frames per channel: 2^28 is 1.4 hours at 48 kHz and 2 GiB of `f32` for
/// the stereo mix, which is held in memory.
pub const MAX_EXPORT_FRAMES: usize = 1 << 28;

/// Seconds rendered after the last clip ends.
pub const TAIL_SECONDS: u32 = 1;

/// Why an export could not render.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportError {
    /// The project could not be compiled.
    Compile(CompileError),
    /// The export would be longer than [`MAX_EXPORT_FRAMES`].
    TooLong {
        /// Frames the export would need.
        frames: u64,
    },
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(error) => error.fmt(f),
            Self::TooLong { frames } => write!(
                f,
                "the export would be {frames} frames long; the limit is {MAX_EXPORT_FRAMES}"
            ),
        }
    }
}

impl std::error::Error for ExportError {}

/// Frames to export at the project's sample rate: from 0 to the end of the last clip (muted
/// ones included) plus [`TAIL_SECONDS`]. A project without clips exports the tail alone.
pub fn export_frames(project: &Project) -> u64 {
    let rate = project.settings.sample_rate;
    let timeline = Timeline::new(
        project.tempo_map.clone(),
        project.time_signature_map.clone(),
    );
    let end = project
        .clips
        .values()
        .map(|clip| timeline.beat_to_sample_index(clip.start + clip.length, rate))
        .max()
        .unwrap_or(0);
    end + u64::from(TAIL_SECONDS) * u64::from(rate)
}

/// The sample index at the start of `beat` at the project's rate (the tempo map applies).
pub fn beat_to_frame(project: &Project, beat: f64) -> u64 {
    Timeline::new(
        project.tempo_map.clone(),
        project.time_signature_map.clone(),
    )
    .beat_to_sample_index(beat.max(0.0), project.settings.sample_rate)
}

/// Renders `frames` frames of `project` from the start at its sample rate and returns them as
/// interleaved stereo (`frames * 2` samples). Allocates and loads media: not for the audio
/// thread; the live engine is never involved.
pub fn render_project(
    project: &Project,
    ctx: &CompileContext,
    frames: usize,
) -> Result<Vec<f32>, ExportError> {
    render_strip(project, ctx, MixerRef::Master, frames)
}

/// Like [`render_project`] for the output of `strip` after its fader (the master's fader gives
/// the whole mix). Sends, buses and inserts upstream of the strip are included; a strip the
/// project does not have is a [`ExportError::Compile`].
pub fn render_strip(
    project: &Project,
    ctx: &CompileContext,
    strip: MixerRef,
    frames: usize,
) -> Result<Vec<f32>, ExportError> {
    if frames > MAX_EXPORT_FRAMES {
        return Err(ExportError::TooLong {
            frames: frames as u64,
        });
    }
    let compiled =
        compile_project_tap(project, ctx, project.settings.sample_rate, MAX_BLOCK, strip)
            .map_err(ExportError::Compile)?;
    let latency = compiled.plan.output_latency() as usize;
    let mut renderer = OfflineRenderer::new(compiled.plan, 2);
    // A fresh renderer's queues have room for these.
    let queued = renderer.link().swap_timeline(compiled.timeline).is_ok()
        && renderer.send(Message::Transport(TransportCommand::Locate(0)))
        && renderer.send(Message::Transport(TransportCommand::Play));
    debug_assert!(queued);
    let mut mix = renderer.render(frames + latency, MAX_BLOCK);
    mix.drain(..latency * 2);
    Ok(mix)
}
