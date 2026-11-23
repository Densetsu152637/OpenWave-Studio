//! Mixing the project down to a WAV file (`export_wav`).

use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use ow_engine::export::{export_frames, render_project};
use ow_ipc::{ExportOptions, ExportResult, MixerRef, NormalizeOptions};
use ow_media::analysis::{Normalization, analyze, normalize};

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
        let options = ExportOptions {
            path: path.to_string_lossy().into_owned(),
            normalize: None,
        };
        self.export_mix(&options).map(|result| result.path)
    }

    /// Like [`export_wav`](Self::export_wav) with options: with `normalize` the rendered mix is
    /// scaled to `target_lufs` integrated loudness, or by less when that would put its true peak
    /// over `true_peak_ceiling_db` (a plain gain: nothing is limited or clipped). Answers where
    /// the file went, what normalisation did and the measurements of what was written. A
    /// lookahead limiter's latency is compensated, so the file starts at the timeline's start.
    pub fn export_mix(&self, options: &ExportOptions) -> Result<ExportResult, String> {
        if let Some(normalize) = &options.normalize
            && !(normalize.target_lufs.is_finite() && normalize.true_peak_ceiling_db.is_finite())
        {
            return Err("the loudness target and the ceiling must be numbers".into());
        }
        let (project, ctx) = {
            let control = super::lock(&self.control);
            (control.session().project().clone(), control.ctx.clone())
        };
        let rate = project.settings.sample_rate;
        let frames = usize::try_from(export_frames(&project)).unwrap_or(usize::MAX);
        let mut mix = render_project(&project, &ctx, frames).map_err(|error| error.to_string())?;
        let normalization = options.normalize.map(|normalize| {
            super::analysis::normalization_report(scale(&mut mix, rate, normalize))
        });
        let path = wav_path(Path::new(&options.path));
        write_wav(&path, rate, &mix)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
        Ok(ExportResult {
            path: path.to_string_lossy().into_owned(),
            sample_rate: rate,
            frames: (mix.len() / 2) as u64,
            normalization,
            analysis: super::analysis::report(MixerRef::Master, 0, &analyze(&mix, rate)),
        })
    }
}

/// Normalises `mix` as `options` ask.
fn scale(mix: &mut [f32], rate: u32, options: NormalizeOptions) -> Normalization {
    normalize(mix, rate, options.target_lufs, options.true_peak_ceiling_db)
}
