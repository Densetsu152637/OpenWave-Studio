//! Wire types for offline analysis and export: what the `analyze` and `export_mix` control
//! functions take and answer. The measurements come from `ow-media::analysis`; the app crate
//! fills these in. Levels that are `None` are digital silence (nothing to report in dB).

use ow_model::MixerRef;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What to analyse: a strip's output and a stretch of the timeline.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(rename_all = "camelCase")]
pub struct AnalysisRequest {
    /// The strip to hear after its fader; `None` is the master (the whole mix).
    pub strip: Option<MixerRef>,
    /// First beat to analyse; `None` is the start of the project.
    pub start_beat: Option<f64>,
    /// Beat the analysis stops at (exclusive); `None` is the end of the last clip plus the
    /// export tail.
    pub end_beat: Option<f64>,
}

/// One analysis band and its energy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct BandEnergy {
    /// Lower edge in hertz.
    pub low_hz: f64,
    /// Upper edge in hertz (the Nyquist frequency for the top band).
    pub high_hz: f64,
    /// Energy in dB relative to a full-scale mean square (a full-scale sine reads -3 dB); the
    /// bands add up to the total.
    pub energy_db: Option<f64>,
}

/// The measurements of one analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct AnalysisReport {
    /// The strip that was analysed.
    pub strip: MixerRef,
    /// Sample rate of the render.
    pub sample_rate: u32,
    /// First frame analysed.
    #[ts(type = "number")]
    pub start_frame: u64,
    /// Frames analysed.
    #[ts(type = "number")]
    pub frames: u64,
    /// Gated integrated loudness (ITU-R BS.1770-4) in LUFS.
    pub integrated_lufs: Option<f64>,
    /// Highest 3 s short-term loudness in LUFS.
    pub short_term_max_lufs: Option<f64>,
    /// Highest 400 ms momentary loudness in LUFS.
    pub momentary_max_lufs: Option<f64>,
    /// True peak (4x oversampled) in dBTP.
    pub true_peak_dbtp: Option<f64>,
    /// Highest absolute sample value in dBFS.
    pub sample_peak_db: Option<f64>,
    /// RMS over both channels in dBFS.
    pub rms_db: Option<f64>,
    /// Sample peak minus RMS in dB.
    pub crest_factor_db: Option<f64>,
    /// Correlation of left and right, -1 to 1 (0 for silence).
    pub correlation: f64,
    /// The eight frequency bands, lowest first.
    pub bands: Vec<BandEnergy>,
}

/// Loudness normalisation applied at export.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct NormalizeOptions {
    /// Integrated loudness to reach, in LUFS.
    pub target_lufs: f64,
    /// True-peak ceiling in dBTP; the gain is lowered when reaching the target would exceed it.
    pub true_peak_ceiling_db: f64,
}

/// What normalisation did.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct NormalizationReport {
    /// Gain applied, in dB.
    pub gain_db: f64,
    /// True when the ceiling, not the target, set the gain (the result is quieter than the
    /// target).
    pub limited_by_ceiling: bool,
    /// Integrated loudness before, in LUFS.
    pub loudness_before: Option<f64>,
    /// Integrated loudness after, in LUFS.
    pub loudness_after: Option<f64>,
    /// True peak after, in dBTP.
    pub true_peak_after: Option<f64>,
}

/// What to export and how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ExportOptions {
    /// The file to write (`.wav` is added when it has no extension).
    pub path: String,
    /// Scale the mix to a loudness first; `None` exports it as rendered.
    #[serde(default)]
    pub normalize: Option<NormalizeOptions>,
}

/// What an export wrote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ExportResult {
    /// The file written.
    pub path: String,
    /// Sample rate of the file.
    pub sample_rate: u32,
    /// Frames written.
    #[ts(type = "number")]
    pub frames: u64,
    /// What normalisation did, when it was asked for.
    pub normalization: Option<NormalizationReport>,
    /// Measurements of what was written (after normalisation).
    pub analysis: AnalysisReport,
}
