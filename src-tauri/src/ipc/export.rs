//! Mixing the project down to a WAV file (`export_wav`).

use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use ow_engine::export::{export_frames, render_project};

use super::IpcState;

/// `path` with `.wav` added when it has no extension.
fn wav_path(path: &Path) -> PathBuf {
    let mut path = path.to_path_buf();
    if path.extension().is_none() {
        path.set_extension("wav");
    }
    path
}

/// Writes interleaved stereo `mix` as a 32-bit float WAV through a temporary file that replaces
/// `path` only once complete.
fn write_wav(path: &Path, rate: u32, mix: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut temp = path.as_os_str().to_owned();
    temp.push(".writing");
    let temp = PathBuf::from(temp);
    let write = || -> Result<(), String> {
        let file = fs::File::create(&temp).map_err(|error| error.to_string())?;
        let mut writer =
            hound::WavWriter::new(BufWriter::new(file), spec).map_err(|error| error.to_string())?;
        for sample in mix {
            writer
                .write_sample(*sample)
                .map_err(|error| error.to_string())?;
        }
        writer.finalize().map_err(|error| error.to_string())?;
        fs::rename(&temp, path).map_err(|error| error.to_string())
    };
    write().inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

impl IpcState {
    /// Renders the project from 0 to the end of its last clip plus one second, at the project's
    /// sample rate, into a 32-bit float stereo WAV at `path` (`.wav` is added when it has no
    /// extension); returns the path written. The render runs offline on its own plan: neither
    /// the audio engine nor the control lock is held while it works.
    pub fn export_wav(&self, path: &Path) -> Result<String, String> {
        let (project, ctx) = {
            let control = super::lock(&self.control);
            (control.session().project().clone(), control.ctx.clone())
        };
        let frames = usize::try_from(export_frames(&project)).unwrap_or(usize::MAX);
        let mix = render_project(&project, &ctx, frames).map_err(|error| error.to_string())?;
        let path = wav_path(path);
        write_wav(&path, project.settings.sample_rate, &mix)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        Ok(path.to_string_lossy().into_owned())
    }
}
