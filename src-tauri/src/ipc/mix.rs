//! Mix analysis and export with normalisation through the control layer (editor goals M-G12
//! and M-G13 at the app level): a project built from imported audio is analysed, and exported
//! to a target loudness, without a window.

use std::f64::consts::TAU;
use std::path::Path;

use ow_ipc::{AnalysisRequest, EditCommand, ExportOptions, MixerRef, NormalizeOptions, UiCommand};
use ow_media::analysis::analyze;
use ow_model::{
    Clip, ClipContent, MediaId, Owned, Strip, StripSettings, Track, TrackId, TrackKind,
};

use super::IpcState;
use super::mvp::{CompilingTarget, command, edit};

const RATE: u32 = 48_000;

/// A mono 1 kHz sine at -20 dBFS peak, `seconds` long.
fn write_tone(path: &Path, seconds: u32) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for frame in 0..seconds * RATE {
        let phase = TAU * 1000.0 * f64::from(frame) / f64::from(RATE);
        writer.write_sample((0.1 * phase.sin()) as f32).unwrap();
    }
    writer.finalize().unwrap();
}

/// A state with one audio track (strip `settings`) playing an 8-beat (4 s) tone clip from beat
/// 0; returns it and the track id.
fn project(work: &Path, settings: StripSettings) -> (IpcState, CompilingTarget, TrackId) {
    let wav = work.join("tone.wav");
    write_tone(&wav, 4);
    let target = CompilingTarget::default();
    let state = IpcState::new();
    let media = state.media_import(&wav, &target).unwrap().media;
    let ids = command(&state, &target, UiCommand::AllocIds { count: 2 });
    let track = TrackId(ids[0]);
    edit(
        &state,
        &target,
        EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned::new(Track {
                id: track,
                name: "Tone".into(),
                color: 0,
                kind: TrackKind::Audio,
                strip: Strip {
                    settings,
                    ..Strip::default()
                },
                instrument: None,
                input: None,
                monitor: Default::default(),
            })),
        },
    );
    edit(
        &state,
        &target,
        EditCommand::AddClip {
            clip: Box::new(Clip {
                id: ids[1].into(),
                track,
                name: "Tone".into(),
                start: 0.0,
                length: 8.0,
                muted: false,
                content: ClipContent::Audio {
                    media: MediaId(media),
                    source_offset: 0,
                    gain_db: 0.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                },
                take: None,
            }),
        },
    );
    (state, target, track)
}

fn near(value: Option<f64>, expected: f64, tolerance: f64, what: &str) {
    let value = value.unwrap_or_else(|| panic!("{what} is silent"));
    assert!(
        (value - expected).abs() <= tolerance,
        "{what}: {value} vs {expected}"
    );
}

#[test]
fn analysis_reports_the_master_a_strip_and_a_range() {
    let work = tempfile::tempdir().unwrap();
    let (state, _target, _) = project(work.path(), StripSettings::default());

    // A mono source at the centre of the pan law is 3 dB under its peak in each channel: the
    // -20 dBFS sine reads -23.01 LUFS and -23.01 dBTP.
    let report = state.analyze(&AnalysisRequest::default()).unwrap();
    assert_eq!(report.strip, MixerRef::Master);
    assert_eq!((report.sample_rate, report.start_frame), (RATE, 0));
    assert_eq!(report.frames, 5 * u64::from(RATE), "4 s plus the 1 s tail");
    // The blocks that straddle the end of the tone are part of the gated mean: 0.16 LU less.
    near(report.integrated_lufs, -23.17, 0.05, "integrated");
    near(report.true_peak_dbtp, -23.01, 0.1, "true peak");
    near(report.sample_peak_db, -23.01, 0.05, "sample peak");
    // Over the whole 5 s: the 4 s of tone and 1 s of silence.
    near(report.rms_db, -26.99, 0.05, "rms");
    near(Some(report.correlation), 1.0, 1e-6, "correlation");
    assert_eq!(report.bands.len(), 8);
    assert_eq!(report.bands[4].low_hz, 1000.0);
    assert_eq!(report.bands[7].high_hz, 24_000.0);
    assert!(report.bands[4].energy_db.unwrap() > report.bands[2].energy_db.unwrap_or(-300.0));

    // One strip after its fader: the pan moves everything to the right.
    let settings = StripSettings {
        pan: 1.0,
        gain_db: -6.0,
        ..StripSettings::default()
    };
    let (state, _target, track) = project(work.path(), settings);
    let strip = state
        .analyze(&AnalysisRequest {
            strip: Some(MixerRef::Track(track)),
            ..AnalysisRequest::default()
        })
        .unwrap();
    assert_eq!(strip.strip, MixerRef::Track(track));
    near(strip.sample_peak_db, -26.0, 0.05, "panned strip peak");

    // A range: beats 2 to 6 (one second in, two seconds long at 120 BPM) of the tone.
    let (state, _target, _) = project(work.path(), StripSettings::default());
    let range = state
        .analyze(&AnalysisRequest {
            strip: None,
            start_beat: Some(2.0),
            end_beat: Some(6.0),
        })
        .unwrap();
    assert_eq!((range.start_frame, range.frames), (48_000, 96_000));
    near(range.integrated_lufs, -23.01, 0.05, "range integrated");

    // Bad ranges and strips are errors, not panics.
    for request in [
        AnalysisRequest {
            start_beat: Some(4.0),
            end_beat: Some(4.0),
            ..AnalysisRequest::default()
        },
        AnalysisRequest {
            start_beat: Some(-1.0),
            ..AnalysisRequest::default()
        },
        AnalysisRequest {
            end_beat: Some(f64::NAN),
            ..AnalysisRequest::default()
        },
        AnalysisRequest {
            strip: Some(MixerRef::Track(TrackId(9_999))),
            ..AnalysisRequest::default()
        },
    ] {
        assert!(state.analyze(&request).is_err(), "{request:?}");
    }
}

#[test]
fn analysis_export_normalises_to_a_loudness_under_a_true_peak_ceiling() {
    let work = tempfile::tempdir().unwrap();
    let (state, _target, _) = project(work.path(), StripSettings::default());
    let options = |target_lufs, ceiling| ExportOptions {
        path: work.path().join("master").to_string_lossy().into_owned(),
        normalize: Some(NormalizeOptions {
            target_lufs,
            true_peak_ceiling_db: ceiling,
        }),
    };

    // -23 LUFS up to -14: +9 dB, peak -14 dBTP, well under the -1 ceiling.
    let result = state.export_mix(&options(-14.0, -1.0)).unwrap();
    assert!(result.path.ends_with("master.wav"));
    let done = result.normalization.expect("normalised");
    assert!(!done.limited_by_ceiling);
    near(Some(done.gain_db), 9.17, 0.05, "gain");
    near(done.loudness_after, -14.0, 0.1, "after");
    near(result.analysis.integrated_lufs, -14.0, 0.1, "reported");
    // The file on disk measures the same.
    let mut reader = hound::WavReader::open(&result.path).unwrap();
    assert_eq!(reader.spec().sample_rate, RATE);
    let samples: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
    assert_eq!(samples.len() as u64, result.frames * 2);
    near(analyze(&samples, RATE).integrated_lufs, -14.0, 0.1, "file");

    // A target that would break the ceiling stops at the ceiling: peak exactly -1 dBTP.
    let result = state.export_mix(&options(0.0, -1.0)).unwrap();
    let done = result.normalization.unwrap();
    assert!(done.limited_by_ceiling);
    near(done.true_peak_after, -1.0, 0.01, "ceiling");
    assert!(done.loudness_after.unwrap() < 0.0);

    // Without options the mix is written as rendered; bad numbers are refused.
    let plain = state
        .export_mix(&ExportOptions {
            path: work.path().join("plain.wav").to_string_lossy().into_owned(),
            normalize: None,
        })
        .unwrap();
    assert!(plain.normalization.is_none());
    near(plain.analysis.integrated_lufs, -23.17, 0.05, "as rendered");
    assert!(state.export_mix(&options(f64::NAN, -1.0)).is_err());
}
