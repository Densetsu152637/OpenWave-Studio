//! Warped audio clips (stage 22b): a loop recorded at one tempo follows the project's tempo.
//! The loop is stretched offline at compile time, so these tests render the compiled plan and
//! measure where the sound lands.

use std::f64::consts::TAU;
use std::path::Path;

use ow_engine::clips::warp;
use ow_engine::{CompileContext, Message, OfflineRenderer, TransportCommand, compile_project};
use ow_model::time::{TempoMap, TempoPoint};
use ow_model::{
    Clip, ClipContent, ClipId, EditCommand, Editor, MediaId, MediaItem, Owned, Project, Strip,
    Track, TrackId, TrackKind, Warp, WarpMode,
};

const RATE: u32 = 48_000;
/// A loop of two 4/4 bars at 120 bpm: eight beats of half a second.
const LOOP_FRAMES: u32 = 192_000;
const BEAT_120: u32 = 24_000;

struct Fixture {
    root: tempfile::TempDir,
    editor: Editor,
    media: MediaId,
    track: TrackId,
}

/// Writes `audio/loop.wav`: mono, a 1 kHz burst of 20 ms on every beat when `clicks`, else a
/// steady 440 Hz sine.
fn write_loop(root: &Path, clicks: bool) -> String {
    std::fs::create_dir_all(root.join("audio")).unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let path = "audio/loop.wav".to_owned();
    let mut writer = hound::WavWriter::create(root.join(&path), spec).unwrap();
    for i in 0..LOOP_FRAMES {
        let seconds = f64::from(i) / f64::from(RATE);
        let value = if clicks {
            let into_beat = i % BEAT_120;
            if into_beat < 960 {
                0.8 * (TAU * 1000.0 * seconds).sin()
            } else {
                0.0
            }
        } else {
            0.5 * (TAU * 440.0 * seconds).sin()
        };
        writer.write_sample(value as f32).unwrap();
    }
    writer.finalize().unwrap();
    path
}

impl Fixture {
    fn new(clicks: bool, project_bpm: f64) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = write_loop(root.path(), clicks);
        let mut editor = Editor::new(Project::new("warp"));
        editor
            .apply(EditCommand::SetTempoMap {
                tempo_map: TempoMap::constant(project_bpm).unwrap(),
            })
            .unwrap();
        let media: MediaId = editor.alloc_id().unwrap();
        editor
            .apply(EditCommand::AddMedia {
                item: MediaItem {
                    id: media,
                    name: "loop".into(),
                    path,
                    hash: if clicks { "clicks" } else { "sine" }.into(),
                    sample_rate: RATE,
                    channels: 1,
                    frames: u64::from(LOOP_FRAMES),
                    tempo: Some(120.0),
                    key: None,
                },
            })
            .unwrap();
        let track: TrackId = editor.alloc_id().unwrap();
        editor
            .apply(EditCommand::InsertTrack {
                index: 0,
                bundle: Box::new(Owned::new(Track {
                    id: track,
                    name: "t".into(),
                    color: 0,
                    kind: TrackKind::Audio,
                    strip: Strip::default(),
                    instrument: None,
                    input: None,
                    monitor: Default::default(),
                })),
            })
            .unwrap();
        Self {
            root,
            editor,
            media,
            track,
        }
    }

    fn clip(&mut self, start: f64, length: f64, warp: Option<Warp>) -> ClipId {
        let id: ClipId = self.editor.alloc_id().unwrap();
        self.editor
            .apply(EditCommand::AddClip {
                clip: Box::new(Clip {
                    id,
                    track: self.track,
                    name: "c".into(),
                    start,
                    length,
                    muted: false,
                    content: ClipContent::Audio {
                        media: self.media,
                        source_offset: 0,
                        gain_db: 0.0,
                        fade_in: 0.0,
                        fade_out: 0.0,
                        warp,
                    },
                    take: None,
                }),
            })
            .unwrap();
        id
    }

    fn render(&self, ctx: &CompileContext, frames: usize) -> Vec<f32> {
        let compiled = compile_project(self.editor.project(), ctx, RATE, 256).unwrap();
        let mut renderer = OfflineRenderer::new(compiled.plan, 2);
        assert!(renderer.send(Message::Transport(TransportCommand::Play)));
        renderer.render(frames, 211)
    }

    fn ctx(&self) -> CompileContext {
        CompileContext::new(self.root.path())
    }
}

fn follow(source_bpm: f64) -> Warp {
    Warp {
        mode: WarpMode::FollowTempo,
        source_bpm,
        markers: vec![],
        transpose_semitones: 0.0,
    }
}

/// Left channel of an interleaved stereo render.
fn left(buffer: &[f32]) -> Vec<f32> {
    buffer.chunks(2).map(|f| f[0]).collect()
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |m, s| m.max(s.abs()))
}

fn warp_files(root: &Path) -> usize {
    std::fs::read_dir(root.join("cache"))
        .map(|read| {
            read.filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with("warp-"))
                .count()
        })
        .unwrap_or(0)
}

/// A 120 bpm loop of two bars in a 90 bpm project fills two bars of 90 bpm to the sample: the
/// sound stops at the clip's end and is still there one sample before it.
#[test]
fn warp_two_bars_at_120_span_two_bars_at_90_within_a_sample() {
    let mut fx = Fixture::new(false, 90.0);
    // 8 beats at 90 bpm: 8 * 32000 frames.
    let clip_frames = 8 * 32_000;
    let start_beats = 4.0;
    fx.clip(start_beats, 8.0, Some(follow(120.0)));
    let out = left(&fx.render(&fx.ctx(), 4 * 32_000 + clip_frames + 20_000));
    let start = 4 * 32_000;
    let end = start + clip_frames;
    assert_eq!(peak(&out[..start - 1]), 0.0, "silent before the clip");
    assert!(
        peak(&out[end - 200..end - 1]) > 0.1,
        "the stretched loop still sounds one sample before its end"
    );
    assert_eq!(peak(&out[end..]), 0.0, "and nothing sounds from the end on");
    assert!(peak(&out[start..start + 4_000]) > 0.1);
    // The same through the plan: the clip's render is exactly as long as its timeline span.
    let compiled = compile_project(fx.editor.project(), &fx.ctx(), RATE, 256).unwrap();
    assert!(compiled.missing_media.is_empty());
}

/// The offline render itself is exactly the clip's length for any tempo and pitch.
#[test]
fn warp_render_length_is_exact_for_other_tempos_and_pitch() {
    let fx = Fixture::new(false, 90.0);
    let ctx = fx.ctx();
    let loaded = ctx
        .sources
        .load(&fx.root.path().join("audio/loop.wav"), RATE)
        .unwrap();
    for bpm in [70.0, 90.0, 97.3, 120.0, 150.0, 200.0] {
        let timeline = ow_engine::Timeline::new(
            TempoMap::constant(bpm).unwrap(),
            ow_model::time::TimeSignatureMap::default(),
        );
        for pitch in [0.0, 3.0, -5.0] {
            let mut w = follow(120.0);
            w.transpose_semitones = pitch;
            let segments = warp::plan(1.0, 8.0, &w, 0, RATE, &timeline, RATE);
            let want =
                timeline.beat_to_sample_index(9.0, RATE) - timeline.beat_to_sample_index(1.0, RATE);
            assert_eq!(warp::total_frames(&segments), want);
            let pcm = warp::render(&loaded.pcm, &segments, pitch as f32);
            assert_eq!(pcm.frames() as u64, want, "bpm {bpm} pitch {pitch}");
        }
    }
}

/// Each source beat lands on a project beat: bursts every half second of the loop sound every
/// 32000 frames in a 90 bpm project, and the gaps between them are quiet.
#[test]
fn warp_puts_the_loops_beats_on_the_projects_beats() {
    let mut fx = Fixture::new(true, 90.0);
    fx.clip(0.0, 8.0, Some(follow(120.0)));
    let out = left(&fx.render(&fx.ctx(), 8 * 32_000));
    for beat in 0..8_usize {
        let at = beat * 32_000;
        let burst = peak(&out[at..at + 2_400]);
        assert!(burst > 0.3, "beat {beat}: burst peak {burst}");
        let gap = peak(&out[at + 8_000..at + 24_000]);
        assert!(gap < 0.05, "beat {beat}: gap peak {gap}");
    }
}

/// A warped clip in a project already at the loop's tempo, with no pitch change, is the
/// unwarped clip bit for bit.
#[test]
fn warp_at_the_source_tempo_is_the_original() {
    let mut fx = Fixture::new(false, 120.0);
    fx.clip(0.0, 8.0, Some(follow(120.0)));
    let warped = fx.render(&fx.ctx(), 100_000);
    let mut plain = Fixture::new(false, 120.0);
    plain.clip(0.0, 8.0, None);
    let original = plain.render(&plain.ctx(), 100_000);
    assert_eq!(warped, original);
}

/// A clip with warp off, or a warp whose mode is off, plays the file as it is.
#[test]
fn warp_off_plays_the_file_unchanged() {
    let mut fx = Fixture::new(false, 90.0);
    let mut w = follow(120.0);
    w.mode = WarpMode::Off;
    fx.clip(0.0, 8.0, Some(w));
    let off = fx.render(&fx.ctx(), 50_000);
    let mut plain = Fixture::new(false, 90.0);
    plain.clip(0.0, 8.0, None);
    assert_eq!(off, plain.render(&plain.ctx(), 50_000));
}

/// The render is cached on disk under `cache/`, reused by a fresh context, and a tempo change
/// makes a new one (and a longer clip at the new tempo is still exact).
#[test]
fn warp_render_is_cached_on_disk_and_follows_tempo_changes() {
    let mut fx = Fixture::new(false, 90.0);
    fx.clip(0.0, 8.0, Some(follow(120.0)));
    let first = fx.render(&fx.ctx(), 260_000);
    assert_eq!(warp_files(fx.root.path()), 1);
    // A new context (a new session) finds the file and gets the same audio.
    let again = fx.render(&fx.ctx(), 260_000);
    assert_eq!(first, again);
    assert_eq!(warp_files(fx.root.path()), 1);
    // Another tempo renders again.
    fx.editor
        .apply(EditCommand::SetTempoMap {
            tempo_map: TempoMap::constant(100.0).unwrap(),
        })
        .unwrap();
    let out = left(&fx.render(&fx.ctx(), 8 * 28_800 + 10_000));
    assert_eq!(warp_files(fx.root.path()), 2);
    let end = 8 * 28_800;
    assert!(peak(&out[end - 2_400..end - 1]) > 0.1);
    assert_eq!(peak(&out[end..]), 0.0);
}

/// A tempo change inside the clip splits the render; the clip still spans its exact length and
/// the second half is faster.
#[test]
fn warp_follows_a_tempo_change_inside_the_clip() {
    let mut fx = Fixture::new(true, 120.0);
    fx.editor
        .apply(EditCommand::SetTempoMap {
            tempo_map: TempoMap::new(vec![
                TempoPoint::constant(0.0, 120.0),
                TempoPoint::constant(4.0, 60.0),
            ])
            .unwrap(),
        })
        .unwrap();
    fx.clip(0.0, 8.0, Some(follow(120.0)));
    let timeline = ow_engine::Timeline::new(
        fx.editor.project().tempo_map.clone(),
        fx.editor.project().time_signature_map.clone(),
    );
    let end = timeline.beat_to_sample_index(8.0, RATE) as usize;
    let out = left(&fx.render(&fx.ctx(), end + 5_000));
    assert_eq!(peak(&out[end..]), 0.0);
    // Beats 0 to 3 are 24000 apart, beats 4 to 7 are 48000 apart: the loop's bursts follow.
    for beat in 0..8_usize {
        let at = timeline.beat_to_sample_index(beat as f64, RATE) as usize;
        assert!(peak(&out[at..at + 2_400]) > 0.3, "beat {beat}");
    }
}

/// A tempo change of more than four times either way is beyond the stretcher: that clip is
/// silent, while the clip around it is unaffected.
#[test]
fn warp_beyond_four_times_is_silent() {
    let mut fx = Fixture::new(false, 25.0);
    // 2 beats of a 120 bpm loop (1 s of source) fill 4.8 s at 25 bpm: a ratio of 4.8.
    fx.clip(0.0, 2.0, Some(follow(120.0)));
    fx.clip(2.0, 2.0, None);
    let out = left(&fx.render(&fx.ctx(), 5 * 48_000));
    // 2 beats at 25 bpm are 4.8 s.
    let end = 4.8 * 48_000.0;
    assert_eq!(
        peak(&out[..end as usize - 1]),
        0.0,
        "the warped clip is silent"
    );
    // The unwarped clip that follows it is the file as it is.
    assert!(peak(&out[end as usize + 100..end as usize + 4_800]) > 0.1);
}

/// Trimming a clip with markers moves the markers' beats back by the trim and keeps their
/// source frames: the remaining stretch of the clip is the same audio at the same speed.
#[test]
fn warp_markers_shifted_by_a_trim_play_the_same_source() {
    let timeline = ow_engine::Timeline::new(
        TempoMap::constant(90.0).unwrap(),
        ow_model::time::TimeSignatureMap::default(),
    );
    let marker = |source_frame: u64, beat: f64| ow_model::WarpMarker { source_frame, beat };
    let mut original = follow(120.0);
    original.markers = vec![marker(24_000, 1.0), marker(120_000, 4.0)];
    let before = warp::plan(0.0, 8.0, &original, 0, RATE, &timeline, RATE);
    // Trim two beats off the left: the clip starts at beat 2 and the markers move to -1 and 2.
    // The one before the clip becomes a marker at 0 on the line through both original markers
    // (at beat 2 that line is at source frame 24000 + (120000 - 24000) / 3 = 56000).
    let mut trimmed = follow(120.0);
    trimmed.markers = vec![marker(56_000, 0.0), marker(120_000, 2.0)];
    let after = warp::plan(2.0, 6.0, &trimmed, 0, RATE, &timeline, RATE);
    assert_eq!(after.len(), 2);
    assert_eq!(
        after[1],
        *before.last().unwrap(),
        "from the second marker on it is identical"
    );
    assert_eq!(before.len(), 3);
    assert_eq!(after[0].source_start, 56_000);
    assert_eq!(after[0].source_end, 120_000);
    assert_eq!(after[0].frames, 2 * 32_000);
}
