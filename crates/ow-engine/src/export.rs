//! Offline export: renders a project's mix without touching the realtime engine.
//!
//! [`render_project`] compiles a plan of its own for the project's sample rate (media comes
//! from the same [`CompileContext`] cache the live engine uses, at that rate), plays the
//! transport from 0 on an [`OfflineRenderer`] and returns the interleaved stereo mix. The
//! caller decides the length; [`export_frames`] gives the usual one, the end of the last clip
//! plus one second for effects tails.

use std::fmt;

use ow_model::Project;

use crate::compile::{CompileContext, CompileError, compile_project};
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

/// Renders `frames` frames of `project` from the start at its sample rate and returns them as
/// interleaved stereo (`frames * 2` samples). Allocates and loads media: not for the audio
/// thread; the live engine is never involved.
pub fn render_project(
    project: &Project,
    ctx: &CompileContext,
    frames: usize,
) -> Result<Vec<f32>, ExportError> {
    if frames > MAX_EXPORT_FRAMES {
        return Err(ExportError::TooLong {
            frames: frames as u64,
        });
    }
    let compiled = compile_project(project, ctx, project.settings.sample_rate, MAX_BLOCK)
        .map_err(ExportError::Compile)?;
    let mut renderer = OfflineRenderer::new(compiled.plan, 2);
    // A fresh renderer's queues have room for these.
    let queued = renderer.link().swap_timeline(compiled.timeline).is_ok()
        && renderer.send(Message::Transport(TransportCommand::Locate(0)))
        && renderer.send(Message::Transport(TransportCommand::Play));
    debug_assert!(queued);
    Ok(renderer.render(frames, MAX_BLOCK))
}
