//! Audio clips through the compiler and the engine cycle (MVP goal M-G1): placement, source
//! offset, clip gain, clip and track mute, track volume, media stored at another sample rate
//! and missing media. Media files are generated here (analytic sines); every render is compared
//! with the closed-form expectation within 1e-5.

use std::f64::consts::TAU;
use std::path::Path;

use ow_engine::export::{ExportError, MAX_EXPORT_FRAMES, export_frames, render_project};
use ow_engine::{
    CompileContext, Message, NodeKey, OfflineRenderer, TransportCommand, compile_project,
};
use ow_model::{
    Clip, ClipContent, EditCommand, Editor, MediaId, MediaItem, Owned, Project, Strip,
    StripSettings, Track, TrackId, TrackKind,
};

/// Gain of a mono source at the centre of the constant-power pan law (-3 dB).
const CENTRE: f64 = std::f64::consts::FRAC_1_SQRT_2;
const RATE: u32 = 48_000;
/// At the default 120 BPM one beat is half a second.
const BEAT: usize = 24_000;
const TOLERANCE: f64 = 1e-5;

/// One channel of a generated media file: `amp * sin(2 pi hz t)`.
#[derive(Clone, Copy)]
struct Tone {
    hz: f64,
    amp: f64,
}

impl Tone {
    /// Value at `seconds`.
    fn at(self, seconds: f64) -> f64 {
        self.amp * (TAU * self.hz * seconds).sin()
    }
}

/// Writes `audio/<name>.wav` (32-bit float) with one tone per channel at `rate`.
fn write_media(root: &Path, name: &str, rate: u32, frames: u32, tones: &[Tone]) -> String {
    std::fs::create_dir_all(root.join("audio")).unwrap();
    let spec = hound::WavSpec {
        channels: tones.len() as u16,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let path = format!("audio/{name}.wav");
    let mut writer = hound::WavWriter::create(root.join(&path), spec).unwrap();
    for i in 0..frames {
        for tone in tones {
            let seconds = f64::from(i) / f64::from(rate);
            writer.write_sample(tone.at(seconds) as f32).unwrap();
        }
    }
    writer.finalize().unwrap();
    path
}

struct Fixture {
    root: tempfile::TempDir,
    editor: Editor,
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
            editor: Editor::new(Project::new("clips")),
        }
    }

    fn ctx(&self) -> CompileContext {
        CompileContext::new(self.root.path())
    }

    /// Writes a media file and adds it to the pool.
    fn media(&mut self, name: &str, rate: u32, frames: u32, tones: &[Tone]) -> MediaId {
        let path = write_media(self.root.path(), name, rate, frames, tones);
        self.pool_item(name, path, rate, frames, tones.len() as u16)
    }

    /// Adds a pool item without writing its file.
    fn pool_item(&mut self, name: &str, path: String, rate: u32, frames: u32, ch: u16) -> MediaId {
        let id = self.editor.alloc_id().unwrap();
        self.editor
            .apply(EditCommand::AddMedia {
                item: MediaItem {
                    id,
                    name: name.into(),
                    path,
                    hash: name.replace('-', "x"),
                    sample_rate: rate,
                    channels: ch,
                    frames: u64::from(frames),
                },
            })
            .unwrap();
        id
    }

    fn track(&mut self, settings: StripSettings) -> TrackId {
        let id: TrackId = self.editor.alloc_id().unwrap();
        let index = self.editor.project().tracks.len();
        self.editor
            .apply(EditCommand::InsertTrack {
                index,
                bundle: Box::new(Owned::new(Track {
                    id,
                    name: "t".into(),
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
            })
            .unwrap();
        id
    }

    /// Adds an audio clip of `media` on `track` and returns its id.
    fn clip(&mut self, track: TrackId, media: MediaId, spec: ClipSpec) -> ow_model::ClipId {
        let id = self.editor.alloc_id().unwrap();
        self.editor
            .apply(EditCommand::AddClip {
                clip: Box::new(Clip {
                    id,
                    track,
                    name: "c".into(),
                    start: spec.start_beats,
                    length: spec.length_beats,
                    muted: spec.muted,
                    content: ClipContent::Audio {
                        media,
                        source_offset: spec.offset,
                        gain_db: spec.gain_db,
                        fade_in: spec.fade_in,
                        fade_out: spec.fade_out,
                    },
                    take: None,
                }),
            })
            .unwrap();
        id
    }

    /// Plays the project from 0 and returns `frames` interleaved stereo frames.
    fn render(&self, ctx: &CompileContext, frames: usize) -> Vec<f32> {
        let compiled = compile_project(self.editor.project(), ctx, RATE, 256).unwrap();
        let mut renderer = OfflineRenderer::new(compiled.plan, 2);
        assert!(renderer.send(Message::Transport(TransportCommand::Play)));
        // An odd block size, so clip edges fall inside blocks.
        renderer.render(frames, 211)
    }
}

#[derive(Clone, Copy)]
struct ClipSpec {
    start_beats: f64,
    length_beats: f64,
    /// First media frame played, counted at the media's own rate.
    offset: u64,
    gain_db: f64,
    muted: bool,
    fade_in: f64,
    fade_out: f64,
}

impl Default for ClipSpec {
    fn default() -> Self {
        Self {
            start_beats: 0.0,
            length_beats: 1.0,
            offset: 0,
            gain_db: 0.0,
            muted: false,
            fade_in: 0.0,
            fade_out: 0.0,
        }
    }
}

fn gain_db(db: f64) -> StripSettings {
    StripSettings {
        gain_db: db,
        ..StripSettings::default()
    }
}

fn linear(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

/// Largest difference between the rendered `channel` (0 left, 1 right) and `expected(frame)`.
fn max_error(out: &[f32], channel: usize, expected: impl Fn(usize) -> f64) -> f64 {
    out.chunks(2)
        .enumerate()
        .map(|(frame, samples)| (f64::from(samples[channel]) - expected(frame)).abs())
        .fold(0.0, f64::max)
}

const LEFT: Tone = Tone {
    hz: 220.0,
    amp: 0.5,
};
const RIGHT: Tone = Tone {
    hz: 330.0,
    amp: 0.25,
};

#[test]
fn a_clip_plays_at_its_position_from_its_source_offset_with_its_gain() {
    let mut fx = Fixture::new();
    let media = fx.media("stereo", RATE, 3 * RATE, &[LEFT, RIGHT]);
    let track = fx.track(StripSettings::default());
    // Half a beat in, one beat long, starting 1000 frames into the file, at -6.0206 dB.
    fx.clip(
        track,
        media,
        ClipSpec {
            start_beats: 0.5,
            length_beats: 1.0,
            offset: 1000,
            gain_db: -6.0206,
            ..ClipSpec::default()
        },
    );
    // A second clip of the same media later in the song, unity gain, from the start of the file.
    fx.clip(
        track,
        media,
        ClipSpec {
            start_beats: 2.0,
            length_beats: 0.5,
            ..ClipSpec::default()
        },
    );
    let ctx = fx.ctx();
    let out = fx.render(&ctx, 3 * BEAT);
    assert_eq!(out.len(), 3 * BEAT * 2);

    let gain = linear(-6.0206);
    let expected = |tone: Tone| {
        move |frame: usize| {
            let first = BEAT / 2;
            let second = 2 * BEAT;
            if (first..first + BEAT).contains(&frame) {
                gain * tone.at((1000 + frame - first) as f64 / f64::from(RATE))
            } else if (second..second + BEAT / 2).contains(&frame) {
                tone.at((frame - second) as f64 / f64::from(RATE))
            } else {
                0.0
            }
        }
    };
    let left = max_error(&out, 0, expected(LEFT));
    let right = max_error(&out, 1, expected(RIGHT));
    assert!(left < TOLERANCE, "left error {left}");
    assert!(right < TOLERANCE, "right error {right}");
    // The clips really are there (and the gaps really are silent).
    assert!(
        out[(BEAT / 2) * 2..(BEAT / 2 + 500) * 2]
            .iter()
            .any(|s| s.abs() > 0.05)
    );
    assert!(out[(BEAT / 4) * 2..BEAT].iter().all(|s| *s == 0.0));
}

#[test]
fn mono_media_plays_on_both_sides() {
    let mut fx = Fixture::new();
    let media = fx.media("mono", RATE, RATE, &[LEFT]);
    let track = fx.track(StripSettings::default());
    fx.clip(track, media, ClipSpec::default());
    let ctx = fx.ctx();
    let out = fx.render(&ctx, BEAT);
    // A mono source sits at the centre of the constant-power pan law: -3 dB on both sides.
    let expected = |frame: usize| LEFT.at(frame as f64 / f64::from(RATE)) * CENTRE;
    assert!(max_error(&out, 0, expected) < TOLERANCE);
    assert!(max_error(&out, 1, expected) < TOLERANCE);
}

#[test]
fn track_volume_track_mute_and_clip_mute_shape_the_mix() {
    let mut fx = Fixture::new();
    let media = fx.media("stereo", RATE, 2 * RATE, &[LEFT, RIGHT]);
    let quiet = fx.track(gain_db(-12.0));
    let loud = fx.track(StripSettings::default());
    let muted_track = fx.track(StripSettings {
        mute: true,
        ..StripSettings::default()
    });
    let clip_muted = fx.track(StripSettings::default());
    fx.clip(quiet, media, ClipSpec::default());
    // Starts a quarter beat later and 2000 frames into the file.
    fx.clip(
        loud,
        media,
        ClipSpec {
            start_beats: 0.25,
            offset: 2000,
            ..ClipSpec::default()
        },
    );
    fx.clip(muted_track, media, ClipSpec::default());
    fx.clip(
        clip_muted,
        media,
        ClipSpec {
            muted: true,
            ..ClipSpec::default()
        },
    );
    let ctx = fx.ctx();
    let out = fx.render(&ctx, BEAT + BEAT / 2);

    let quiet_gain = linear(-12.0);
    let expected = |tone: Tone| {
        move |frame: usize| {
            let seconds = |f: usize| f as f64 / f64::from(RATE);
            let mut sum = 0.0;
            if frame < BEAT {
                sum += quiet_gain * tone.at(seconds(frame));
            }
            let late = BEAT / 4;
            if (late..late + BEAT).contains(&frame) {
                sum += tone.at(seconds(2000 + frame - late));
            }
            sum
        }
    };
    let left = max_error(&out, 0, expected(LEFT));
    let right = max_error(&out, 1, expected(RIGHT));
    assert!(left < TOLERANCE, "left error {left}");
    assert!(right < TOLERANCE, "right error {right}");

    // The muted clip got no player at all; the muted track's clip is silenced by its fader.
    let compiled = compile_project(fx.editor.project(), &ctx, RATE, 256).unwrap();
    let clip_ids: Vec<_> = fx.editor.project().clips.keys().copied().collect();
    let played = clip_ids
        .iter()
        .filter(|id| compiled.nodes.get(NodeKey::Clip(**id)).is_some())
        .count();
    assert_eq!(played, 3, "three of the four clips have players");
    assert!(compiled.missing_media.is_empty());
}

#[test]
fn media_at_another_rate_is_resampled_and_its_source_offset_scales() {
    // A 10 Hz sine: the resampler's sub-sample accuracy (about 0.01 frame, see the stage 11
    // tests) is below 1e-5 there, while an offset that is off by one frame still shows as 6e-4.
    const MEDIA_RATE: u32 = 44_100;
    let tone = Tone { hz: 10.0, amp: 0.5 };
    let mut fx = Fixture::new();
    let media = fx.media("cd", MEDIA_RATE, 3 * MEDIA_RATE, &[tone]);
    let track = fx.track(StripSettings::default());
    // Half a second into the file (22050 frames at its own rate), played from beat 1.
    fx.clip(
        track,
        media,
        ClipSpec {
            start_beats: 1.0,
            length_beats: 2.0,
            offset: u64::from(MEDIA_RATE) / 2,
            ..ClipSpec::default()
        },
    );
    let ctx = fx.ctx();
    let out = fx.render(&ctx, 4 * BEAT);
    let expected = |frame: usize| {
        if (BEAT..3 * BEAT).contains(&frame) {
            tone.at(0.5 + (frame - BEAT) as f64 / f64::from(RATE)) * CENTRE
        } else {
            0.0
        }
    };
    let error = max_error(&out, 0, expected);
    assert!(error < TOLERANCE, "resampled error {error}");
    assert!(max_error(&out, 1, expected) < TOLERANCE);
    // The sine really plays (a silent render would match the gaps only).
    assert!(
        out[BEAT * 2..(BEAT + 6000) * 2]
            .iter()
            .any(|s| s.abs() > 0.1)
    );
    // One resampled copy at the engine rate is cached.
    assert_eq!(ctx.sources.len(), 1);
}

#[test]
fn missing_media_is_silence_and_a_warning_not_an_error() {
    let mut fx = Fixture::new();
    let present = fx.media("here", RATE, RATE, &[LEFT]);
    let gone = fx.pool_item("gone", "audio/gone.wav".into(), RATE, RATE, 1);
    let unmuted_gone = fx.pool_item("alsogone", "audio/alsogone.wav".into(), RATE, RATE, 1);
    let track = fx.track(StripSettings::default());
    fx.clip(track, present, ClipSpec::default());
    fx.clip(
        track,
        gone,
        ClipSpec {
            start_beats: 1.0,
            ..ClipSpec::default()
        },
    );
    // A muted clip of missing media is not a problem: nothing wants to play it.
    fx.clip(
        track,
        unmuted_gone,
        ClipSpec {
            start_beats: 2.0,
            muted: true,
            ..ClipSpec::default()
        },
    );
    let ctx = fx.ctx();
    let compiled = compile_project(fx.editor.project(), &ctx, RATE, 256).unwrap();
    assert_eq!(compiled.missing_media, [gone]);

    let out = fx.render(&ctx, 3 * BEAT);
    let expected = |frame: usize| {
        if frame < BEAT {
            LEFT.at(frame as f64 / f64::from(RATE)) * CENTRE
        } else {
            0.0
        }
    };
    assert!(max_error(&out, 0, expected) < TOLERANCE);
    assert!(
        out[BEAT * 2..].iter().all(|s| *s == 0.0),
        "the missing clip is silent"
    );

    // A corrupt file counts as missing too.
    std::fs::write(fx.root.path().join("audio/gone.wav"), b"not a wav").unwrap();
    let compiled = compile_project(fx.editor.project(), &ctx, RATE, 256).unwrap();
    assert_eq!(compiled.missing_media, [gone]);
}

#[test]
fn recompiling_reuses_loaded_media() {
    let mut fx = Fixture::new();
    let media = fx.media("cached", RATE, RATE, &[LEFT]);
    let track = fx.track(StripSettings::default());
    fx.clip(track, media, ClipSpec::default());
    let ctx = fx.ctx();
    let first = fx.render(&ctx, BEAT);
    assert_eq!(ctx.sources.len(), 1);

    // With the file gone the cached PCM still plays; a fresh cache reports it missing.
    std::fs::remove_file(fx.root.path().join("audio/cached.wav")).unwrap();
    assert_eq!(fx.render(&ctx, BEAT), first);
    let fresh = fx.ctx();
    let compiled = compile_project(fx.editor.project(), &fresh, RATE, 256).unwrap();
    assert_eq!(compiled.missing_media, [media]);

    // Leaving the root drops its entries.
    ctx.sources.retain_under(&fx.root.path().join("elsewhere"));
    assert!(ctx.sources.is_empty());
}

#[test]
fn export_renders_the_mix_offline_to_the_last_clip_end_plus_a_second() {
    let mut fx = Fixture::new();
    let media = fx.media("stereo", RATE, RATE, &[LEFT, RIGHT]);
    let track = fx.track(gain_db(-6.0206));
    fx.clip(
        track,
        media,
        ClipSpec {
            start_beats: 0.5,
            length_beats: 1.5,
            ..ClipSpec::default()
        },
    );
    // A muted clip still counts for the length: the arrangement ends where its clips end.
    fx.clip(
        track,
        media,
        ClipSpec {
            start_beats: 3.0,
            muted: true,
            ..ClipSpec::default()
        },
    );
    let project = fx.editor.project();
    let frames = export_frames(project) as usize;
    assert_eq!(frames, 4 * BEAT + RATE as usize);

    let ctx = fx.ctx();
    let mix = render_project(project, &ctx, frames).unwrap();
    assert_eq!(mix.len(), frames * 2);
    let gain = linear(-6.0206);
    let first = BEAT / 2;
    let error = max_error(&mix, 0, |frame| {
        if (first..first + BEAT * 3 / 2).contains(&frame) {
            gain * LEFT.at((frame - first) as f64 / f64::from(RATE))
        } else {
            0.0
        }
    });
    assert!(error < TOLERANCE, "export error {error}");
    // The same plan rendered on the playback path gives the same samples.
    assert_eq!(mix[..3 * BEAT * 2], fx.render(&ctx, 3 * BEAT)[..]);

    // Too long an export is refused instead of allocating it; a broken project is an error.
    assert!(matches!(
        render_project(project, &ctx, MAX_EXPORT_FRAMES + 1),
        Err(ExportError::TooLong { .. })
    ));
    let mut broken = project.clone();
    broken.settings.sample_rate = 0;
    assert!(matches!(
        render_project(&broken, &ctx, 10),
        Err(ExportError::Compile(_))
    ));
}

/// Linear fade envelope of a `length`-frame clip at `index` with ramps of `fade_in`/`fade_out`
/// frames (the engine's documented curve).
fn envelope(index: usize, length: usize, fade_in: usize, fade_out: usize) -> f64 {
    let mut level = 1.0_f64;
    if index < fade_in {
        level = index as f64 / fade_in as f64;
    }
    if length - index <= fade_out {
        level = level.min((length - index) as f64 / fade_out as f64);
    }
    level
}

#[test]
fn fades_ramp_the_clip_linearly_in_and_out() {
    let mut fx = Fixture::new();
    let media = fx.media("mono", RATE, 2 * RATE, &[LEFT]);
    let track = fx.track(StripSettings::default());
    // One beat long from beat 1, 0.25 beat fade-in (6000 frames), 0.5 beat fade-out (12000).
    fx.clip(
        track,
        media,
        ClipSpec {
            start_beats: 1.0,
            fade_in: 0.25,
            fade_out: 0.5,
            ..ClipSpec::default()
        },
    );
    let ctx = fx.ctx();
    let out = fx.render(&ctx, 3 * BEAT);
    let expected = |frame: usize| {
        if !(BEAT..2 * BEAT).contains(&frame) {
            return 0.0;
        }
        let i = frame - BEAT;
        CENTRE * LEFT.at(i as f64 / f64::from(RATE)) * envelope(i, BEAT, BEAT / 4, BEAT / 2)
    };
    assert!(max_error(&out, 0, expected) < TOLERANCE);
    assert!(max_error(&out, 1, expected) < TOLERANCE);
    // The ramps are really there: the first frame is silent and the last is nearly so.
    assert_eq!(out[BEAT * 2], 0.0);
    assert!(out[(2 * BEAT - 1) * 2].abs() < 1e-3);
}

#[test]
fn fades_that_overlap_clamp_to_the_clip_length() {
    let mut fx = Fixture::new();
    let media = fx.media("mono", RATE, 2 * RATE, &[LEFT]);
    let track = fx.track(StripSettings::default());
    // 0.8 + 0.8 beats on a one-beat clip: the fade-in keeps 0.8, the fade-out gets 0.2.
    fx.clip(
        track,
        media,
        ClipSpec {
            fade_in: 0.8,
            fade_out: 0.8,
            ..ClipSpec::default()
        },
    );
    let ctx = fx.ctx();
    let out = fx.render(&ctx, BEAT);
    let fade_in = BEAT * 4 / 5;
    let expected = |frame: usize| {
        CENTRE
            * LEFT.at(frame as f64 / f64::from(RATE))
            * envelope(frame, BEAT, fade_in, BEAT - fade_in)
    };
    assert!(max_error(&out, 0, expected) < TOLERANCE);
}

#[test]
fn a_left_trimmed_clip_plays_the_same_audio_as_the_untrimmed_clip_from_that_point() {
    let mut fx = Fixture::new();
    let media = fx.media("mono", RATE, 4 * RATE, &[LEFT]);
    let track = fx.track(StripSettings::default());
    // Untrimmed: beat 1 for 2 beats from the file start. Trimmed by half a beat (12000
    // frames at the media rate): start moves to 1.5, the offset advances by 12000.
    fx.clip(
        track,
        media,
        ClipSpec {
            start_beats: 1.5,
            length_beats: 1.5,
            offset: (BEAT / 2) as u64,
            fade_in: 0.25,
            ..ClipSpec::default()
        },
    );
    let ctx = fx.ctx();
    let out = fx.render(&ctx, 4 * BEAT);
    let start = BEAT + BEAT / 2;
    let length = BEAT + BEAT / 2;
    let expected = |frame: usize| {
        if !(start..start + length).contains(&frame) {
            return 0.0;
        }
        let i = frame - start;
        // Same source time as the untrimmed clip would have at this frame.
        CENTRE * LEFT.at((frame - BEAT) as f64 / f64::from(RATE)) * envelope(i, length, BEAT / 4, 0)
    };
    assert!(max_error(&out, 0, expected) < TOLERANCE);
    assert!(out[..start * 2].iter().all(|s| *s == 0.0));
}

#[test]
fn a_right_trimmed_clip_stops_early_and_a_fade_out_ends_at_the_new_edge() {
    let mut fx = Fixture::new();
    let media = fx.media("mono", RATE, 4 * RATE, &[LEFT]);
    let track = fx.track(StripSettings::default());
    fx.clip(
        track,
        media,
        ClipSpec {
            length_beats: 0.5,
            fade_out: 0.25,
            ..ClipSpec::default()
        },
    );
    let ctx = fx.ctx();
    let out = fx.render(&ctx, 2 * BEAT);
    let length = BEAT / 2;
    let expected = |frame: usize| {
        if frame >= length {
            return 0.0;
        }
        CENTRE * LEFT.at(frame as f64 / f64::from(RATE)) * envelope(frame, length, 0, BEAT / 4)
    };
    assert!(max_error(&out, 0, expected) < TOLERANCE);
    assert!(out[length * 2..].iter().all(|s| *s == 0.0));
}
