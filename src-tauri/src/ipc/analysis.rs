//! Offline analysis of the mix (`analyze`) and the report types shared with export.
//!
//! [`IpcState::analyze`] renders the project (the master, or one strip after its fader) through
//! `ow_engine::export` on a plan of its own and measures it with `ow_media::analysis`; the live
//! engine and the control lock are not held while it works. It is a plain method (testable
//! without a window, and what an MCP tool wraps); the Tauri command only hands it the request.

use ow_engine::export::{beat_to_frame, export_frames, render_strip};
use ow_ipc::{AnalysisReport, AnalysisRequest, BandEnergy, MixerRef, NormalizationReport};
use ow_media::analysis::{Analysis, BAND_EDGES_HZ, Normalization, analyze};

use super::IpcState;

/// The wire report for `analysis` of `strip`, whose first frame is `start_frame`.
pub(super) fn report(strip: MixerRef, start_frame: u64, analysis: &Analysis) -> AnalysisReport {
    let nyquist = f64::from(analysis.sample_rate) / 2.0;
    let bands = BAND_EDGES_HZ
        .iter()
        .enumerate()
        .map(|(index, low)| BandEnergy {
            low_hz: *low,
            high_hz: BAND_EDGES_HZ.get(index + 1).copied().unwrap_or(nyquist),
            energy_db: analysis.band_energies_db[index],
        })
        .collect();
    AnalysisReport {
        strip,
        sample_rate: analysis.sample_rate,
        start_frame,
        frames: analysis.frames,
        integrated_lufs: analysis.integrated_lufs,
        short_term_max_lufs: analysis.short_term_max_lufs,
        momentary_max_lufs: analysis.momentary_max_lufs,
        true_peak_dbtp: analysis.true_peak_dbtp,
        sample_peak_db: analysis.sample_peak_db,
        rms_db: analysis.rms_db,
        crest_factor_db: analysis.crest_factor_db,
        correlation: analysis.correlation,
        bands,
    }
}

/// The wire form of what normalisation did.
pub(super) fn normalization_report(done: Normalization) -> NormalizationReport {
    NormalizationReport {
        gain_db: done.gain_db,
        limited_by_ceiling: done.limited_by_ceiling,
        loudness_before: done.loudness_before,
        loudness_after: done.loudness_after,
        true_peak_after: done.true_peak_after,
    }
}

impl IpcState {
    /// Renders the project offline and measures it: loudness (integrated, short-term and
    /// momentary maxima), true and sample peak, RMS, crest factor, stereo correlation and eight
    /// band energies. `request.strip` picks what is heard (the master by default, otherwise one
    /// strip after its fader); `start_beat` and `end_beat` pick the stretch (by default from 0 to
    /// the end of the last clip plus the export tail). The render always runs from the project
    /// start, so tails and held notes are right at the start of a range.
    pub fn analyze(&self, request: &AnalysisRequest) -> Result<AnalysisReport, String> {
        let (project, ctx) = super::lock(&self.control).offline_snapshot();
        let beat = |name: &str, value: Option<f64>| match value {
            Some(beat) if !beat.is_finite() || beat < 0.0 => {
                Err(format!("{name} must be a beat of 0 or more"))
            }
            other => Ok(other),
        };
        let start = beat("start_beat", request.start_beat)?;
        let end = beat("end_beat", request.end_beat)?;
        let strip = request.strip.unwrap_or(MixerRef::Master);
        let first = start.map_or(0, |b| beat_to_frame(&project, b));
        let last = end.map_or_else(|| export_frames(&project), |b| beat_to_frame(&project, b));
        if last <= first {
            return Err("the range to analyse is empty".into());
        }
        let last_index = usize::try_from(last).map_err(|_| "the range is too long".to_string())?;
        let mix = render_strip(&project, &ctx, strip, last_index).map_err(|e| e.to_string())?;
        let range = &mix[first as usize * 2..];
        Ok(report(
            strip,
            first,
            &analyze(range, project.settings.sample_rate),
        ))
    }
}
