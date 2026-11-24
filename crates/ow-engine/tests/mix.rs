//! Mixing through the compiler and the offline renderer (editor goal M-G12): the pan law,
//! balance, solo, buses and pre/post sends, the master insert chain, and a lookahead limiter
//! whose latency is compensated.

use std::f64::consts::{FRAC_1_SQRT_2, TAU};
use std::path::Path;

use ow_engine::export::{render_project, render_strip};
use ow_engine::{CompileContext, compile_project, pan_gains};
use ow_model::{
    AuxSend, Bus, BusId, Clip, ClipContent, Device, DeviceId, EditCommand, Editor, MediaId,
    MediaItem, MixerRef, Output, Owned, Project, Strip, StripSettings, Track, TrackId, TrackKind,
};

const RATE: u32 = 48_000;
const FRAMES: usize = 24_000;
/// Samples ignored at the start of a measurement, so filters and the limiter have settled.
const SETTLE: usize = 4_000;
const LIMITER: &str = "builtin.limiter";
const EQ: &str = "builtin.eq";

fn write_wav(path: &Path, channels: u16, frames: u32, sample: impl Fn(u32, u16) -> f32) {
    let spec = hound::WavSpec {
        channels,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for frame in 0..frames {
        for channel in 0..channels {
            writer.write_sample(sample(frame, channel)).unwrap();
        }
    }
    writer.finalize().unwrap();
}

fn sine(frame: u32, freq: f64, amplitude: f64) -> f32 {
    (amplitude * (TAU * freq * f64::from(frame) / f64::from(RATE)).sin()) as f32
}

/// Media files: `mono` (440 Hz, 0.2), `stereo` (left 440 Hz at 0.2, right 660 Hz at 0.1),
/// `impulse` (a 0.3 click on frame 100) and `loud` (220 Hz at 0.9), one second each.
struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("audio")).unwrap();
        let audio = root.path().join("audio");
        write_wav(&audio.join("mono.wav"), 1, RATE, |f, _| sine(f, 440.0, 0.2));
        write_wav(&audio.join("stereo.wav"), 2, RATE, |f, c| {
            if c == 0 {
                sine(f, 440.0, 0.2)
            } else {
                sine(f, 660.0, 0.1)
            }
        });
        write_wav(&audio.join("impulse.wav"), 1, RATE, |f, _| {
            if f == 100 { 0.3 } else { 0.0 }
        });
        write_wav(&audio.join("loud.wav"), 1, RATE, |f, _| sine(f, 220.0, 0.9));
        Self { root }
    }

    fn ctx(&self) -> CompileContext {
        CompileContext::new(self.root.path())
    }
}

struct Mix {
    editor: Editor,
}

impl Mix {
    fn new() -> Self {
        Self {
            editor: Editor::new(Project::new("mix")),
        }
    }

    fn project(&self) -> &Project {
        self.editor.project()
    }

    fn media(&mut self, name: &str, channels: u16) -> MediaId {
        let id = self.editor.alloc_id().unwrap();
        self.editor
            .apply(EditCommand::AddMedia {
                item: MediaItem {
                    id,
                    name: name.into(),
                    path: format!("audio/{name}.wav"),
                    hash: "00".into(),
                    sample_rate: RATE,
                    channels,
                    frames: u64::from(RATE),
                },
            })
            .unwrap();
        id
    }

    /// An audio track playing `media` from beat 0 with `settings`.
    fn track(&mut self, media: MediaId, settings: StripSettings) -> TrackId {
        let id: TrackId = self.editor.alloc_id().unwrap();
        let clip = self.editor.alloc_id().unwrap();
        let index = self.project().tracks.len();
        self.editor
            .apply(EditCommand::InsertTrack {
                index,
                bundle: Box::new(Owned {
                    item: Track {
                        id,
                        name: "t".into(),
                        color: 0,
                        kind: TrackKind::Audio,
                        strip: Strip {
                            settings,
                            ..Strip::default()
                        },
                        instrument: None,
                    },
                    clips: vec![Clip {
                        id: clip,
                        track: id,
                        name: "c".into(),
                        start: 0.0,
                        length: 4.0,
                        muted: false,
                        content: ClipContent::Audio {
                            media,
                            source_offset: 0,
                            gain_db: 0.0,
                            fade_in: 0.0,
                            fade_out: 0.0,
                        },
                    }],
                    devices: vec![],
                    lanes: vec![],
                }),
            })
            .unwrap();
        id
    }

    fn bus(&mut self, settings: StripSettings) -> BusId {
        let id: BusId = self.editor.alloc_id().unwrap();
        let index = self.project().buses.len();
        self.editor
            .apply(EditCommand::InsertBus {
                index,
                bundle: Box::new(Owned {
                    item: Bus {
                        id,
                        name: "bus".into(),
                        color: 0,
                        strip: Strip {
                            settings,
                            ..Strip::default()
                        },
                    },
                    clips: vec![],
                    devices: vec![],
                    lanes: vec![],
                }),
            })
            .unwrap();
        id
    }

    /// Appends an insert of `kind` with `params` to `strip`.
    fn insert(&mut self, strip: MixerRef, kind: &str, params: &[(u32, f64)]) -> DeviceId {
        let id: DeviceId = self.editor.alloc_id().unwrap();
        let index = self.project().strip(strip).unwrap().inserts.len();
        self.editor
            .apply(EditCommand::InsertDevice {
                slot: ow_model::DeviceSlot::Insert { strip, index },
                bundle: Box::new(ow_model::DeviceBundle {
                    device: Device {
                        id,
                        kind: kind.into(),
                        name: kind.into(),
                        enabled: true,
                        params: params.iter().copied().collect(),
                        state: vec![],
                        sidechain: None,
                    },
                    lanes: vec![],
                }),
            })
            .unwrap();
        id
    }

    fn settings(&mut self, strip: MixerRef, settings: StripSettings) {
        self.editor
            .apply(EditCommand::SetStripSettings {
                target: strip,
                settings,
            })
            .unwrap();
    }

    fn sends(&mut self, strip: MixerRef, sends: Vec<AuxSend>) {
        self.editor
            .apply(EditCommand::SetSends {
                target: strip,
                sends,
            })
            .unwrap();
    }
}

fn gain(db: f64) -> StripSettings {
    StripSettings {
        gain_db: db,
        ..StripSettings::default()
    }
}

fn panned(pan: f64) -> StripSettings {
    StripSettings {
        pan,
        ..StripSettings::default()
    }
}

fn render(fixture: &Fixture, project: &Project) -> Vec<f32> {
    render_project(project, &fixture.ctx(), FRAMES).unwrap()
}

/// RMS of one channel after the settling time.
fn rms(signal: &[f32], channel: usize) -> f64 {
    let samples: Vec<f64> = signal
        .chunks(2)
        .skip(SETTLE)
        .map(|frame| f64::from(frame[channel]))
        .collect();
    (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt()
}

fn peak(signal: &[f32]) -> f32 {
    signal.iter().fold(0.0, |m, s| m.max(s.abs()))
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

/// RMS of the 0.2-amplitude 440 Hz sine.
const SINE_RMS: f64 = 0.2 * FRAC_1_SQRT_2;

fn near(value: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (value - expected).abs() <= tolerance,
        "{what}: {value} vs {expected}"
    );
}

#[test]
fn mix_pan_law_gains() {
    // Constant power for a mono source: -3.01 dB at the centre, unity on the panned side, silent
    // on the other, and the summed power is constant everywhere.
    let [l, r] = pan_gains(0.0, true);
    near(db(f64::from(l)), -3.0103, 1e-4, "centre left");
    near(db(f64::from(r)), -3.0103, 1e-4, "centre right");
    near(f64::from(pan_gains(-1.0, true)[0]), 1.0, 1e-6, "hard left");
    near(
        f64::from(pan_gains(-1.0, true)[1]),
        0.0,
        1e-6,
        "hard left, right side",
    );
    near(f64::from(pan_gains(1.0, true)[1]), 1.0, 1e-6, "hard right");
    for step in 0..=20 {
        let pan = -1.0 + f64::from(step) * 0.1;
        let [l, r] = pan_gains(pan, true);
        near(f64::from(l * l + r * r), 1.0, 1e-5, "constant power");
    }
    // Balance for stereo sources: unity at the centre, the far side attenuated by cos.
    assert_eq!(pan_gains(0.0, false), [1.0, 1.0]);
    near(
        f64::from(pan_gains(0.5, false)[0]),
        FRAC_1_SQRT_2,
        1e-6,
        "balance right",
    );
    assert_eq!(pan_gains(0.5, false)[1], 1.0);
    assert!(pan_gains(1.0, false)[0].abs() < 1e-6);
    // Junk reads as the centre; out-of-range values are limited.
    assert_eq!(pan_gains(f64::NAN, false), [1.0, 1.0]);
    assert_eq!(pan_gains(9.0, true), pan_gains(1.0, true));
}

#[test]
fn mix_pan_moves_a_mono_track_by_the_constant_power_law() {
    let fixture = Fixture::new();
    for (pan, left, right) in [
        (0.0, FRAC_1_SQRT_2, FRAC_1_SQRT_2),
        (-1.0, 1.0, 0.0),
        (1.0, 0.0, 1.0),
        (0.5, 0.382_683_4, 0.923_879_5),
    ] {
        let mut mix = Mix::new();
        let media = mix.media("mono", 1);
        mix.track(media, panned(pan));
        let out = render(&fixture, mix.project());
        near(
            rms(&out, 0),
            SINE_RMS * left,
            2e-4,
            &format!("pan {pan} left"),
        );
        near(
            rms(&out, 1),
            SINE_RMS * right,
            2e-4,
            &format!("pan {pan} right"),
        );
    }
}

#[test]
fn mix_pan_balances_a_stereo_track() {
    let fixture = Fixture::new();
    let sides = |pan: f64| {
        let mut mix = Mix::new();
        let media = mix.media("stereo", 2);
        mix.track(media, panned(pan));
        let out = render(&fixture, mix.project());
        (rms(&out, 0), rms(&out, 1))
    };
    // Unity on both sides at the centre.
    let (left, right) = sides(0.0);
    near(left, SINE_RMS, 2e-4, "centre left");
    near(right, 0.1 * FRAC_1_SQRT_2, 2e-4, "centre right");
    // Panning right keeps the right side and attenuates the left; hard right silences it.
    let (left, right) = sides(0.5);
    near(left, SINE_RMS * FRAC_1_SQRT_2, 2e-4, "right 0.5, left");
    near(right, 0.1 * FRAC_1_SQRT_2, 2e-4, "right 0.5, right");
    let (left, _) = sides(1.0);
    assert!(left < 1e-6, "{left}");
    let (_, right) = sides(-1.0);
    assert!(right < 1e-6, "{right}");
}

#[test]
fn mix_solo_silences_the_other_tracks_unless_solo_safe() {
    let fixture = Fixture::new();
    let mut mix = Mix::new();
    let media = mix.media("mono", 1);
    let a = mix.track(media, gain(0.0));
    let b = mix.track(media, gain(0.0));
    let c = mix.track(
        media,
        StripSettings {
            solo_safe: true,
            ..StripSettings::default()
        },
    );
    let both = render(&fixture, mix.project());
    near(
        rms(&both, 0),
        3.0 * SINE_RMS * FRAC_1_SQRT_2,
        1e-3,
        "three tracks",
    );

    let mut soloed = mix.project().clone();
    soloed.tracks[0].strip.settings.solo = true;
    let out = render(&fixture, &soloed);
    near(
        rms(&out, 0),
        2.0 * SINE_RMS * FRAC_1_SQRT_2,
        1e-3,
        "solo plus solo-safe",
    );
    soloed.tracks[2].strip.settings.solo_safe = false;
    let out = render(&fixture, &soloed);
    near(rms(&out, 0), SINE_RMS * FRAC_1_SQRT_2, 1e-3, "solo alone");
    let _ = (a, c);
    // The strip tap hears the same: track b is silenced by a's solo.
    let b_alone = render_strip(&soloed, &fixture.ctx(), MixerRef::Track(b), FRAMES).unwrap();
    assert_eq!(peak(&b_alone), 0.0);
}

#[test]
fn mix_buses_sum_their_tracks_and_apply_their_fader() {
    let fixture = Fixture::new();
    let mut mix = Mix::new();
    let media = mix.media("mono", 1);
    let drums = mix.bus(gain(-6.0206));
    let to_bus = StripSettings {
        output: Output::Bus(drums),
        ..StripSettings::default()
    };
    mix.track(media, to_bus);
    mix.track(media, to_bus);
    mix.track(media, StripSettings::default());
    let out = render(&fixture, mix.project());
    // Two tracks through a -6 dB bus (one half in total) plus one straight to the master.
    near(
        rms(&out, 0),
        SINE_RMS * FRAC_1_SQRT_2 * (1.0 + 2.0 * 0.5),
        1e-3,
        "bus sum",
    );
}

#[test]
fn mix_sends_apply_their_level_and_tap_pre_or_post_fader() {
    let fixture = Fixture::new();
    let mut mix = Mix::new();
    let media = mix.media("mono", 1);
    let aux = mix.bus(gain(0.0));
    let track = mix.track(media, gain(-100.0));
    let send = |level_db, pre_fader| AuxSend {
        target: aux,
        level_db,
        pre_fader,
    };
    let source = SINE_RMS * FRAC_1_SQRT_2;
    // Post-fader: a track whose fader is nearly closed sends nothing.
    mix.sends(MixerRef::Track(track), vec![send(0.0, false)]);
    assert!(rms(&render(&fixture, mix.project()), 0) < 1e-4);
    // Pre-fader: the send ignores the fader and applies its own level (-6 dB: half).
    mix.sends(MixerRef::Track(track), vec![send(-6.0206, true)]);
    near(
        rms(&render(&fixture, mix.project()), 0),
        source * 0.5,
        1e-3,
        "pre-fader send",
    );
    // Two sends add: -6 dB pre plus -6 dB pre.
    mix.sends(
        MixerRef::Track(track),
        vec![send(-6.0206, true), send(-6.0206, true)],
    );
    near(
        rms(&render(&fixture, mix.project()), 0),
        source,
        1e-3,
        "two sends",
    );
    // Post-fader sends follow the fader.
    let mut project = mix.project().clone();
    project.tracks[0].strip.settings.gain_db = -6.0206;
    project.tracks[0].strip.sends = vec![send(-6.0206, false)];
    near(
        rms(&render(&fixture, &project), 0),
        source * 0.5 * 0.5 + source * 0.5,
        2e-3,
        "post",
    );
    // A send to a bus that is itself muted is silent.
    project.buses[0].strip.settings.mute = true;
    project.tracks[0].strip.settings.gain_db = -100.0;
    assert!(rms(&render(&fixture, &project), 0) < 1e-4);
}

#[test]
fn mix_a_bus_without_inserts_sends_pre_fader_from_its_input() {
    // Track -> bus A (fader closed) with a pre-fader send to bus B: B still hears the track.
    let fixture = Fixture::new();
    let mut mix = Mix::new();
    let media = mix.media("mono", 1);
    let a = mix.bus(gain(-100.0));
    let b = mix.bus(gain(0.0));
    mix.track(
        media,
        StripSettings {
            output: Output::Bus(a),
            ..StripSettings::default()
        },
    );
    mix.sends(
        MixerRef::Bus(a),
        vec![AuxSend {
            target: b,
            level_db: 0.0,
            pre_fader: true,
        }],
    );
    // Bus ids: a < b, so the routing is acyclic.
    near(
        rms(&render(&fixture, mix.project()), 0),
        SINE_RMS * FRAC_1_SQRT_2,
        1e-3,
        "pre-fader send from a bus",
    );
    // Post-fader, the closed fader silences it.
    let mut project = mix.project().clone();
    project.buses[0].strip.sends[0].pre_fader = false;
    assert!(rms(&render(&fixture, &project), 0) < 1e-4);
}

#[test]
fn mix_master_inserts_process_the_summed_mix() {
    let fixture = Fixture::new();
    let mut mix = Mix::new();
    let media = mix.media("mono", 1);
    mix.track(media, gain(0.0));
    let dry = render(&fixture, mix.project());
    // A +6 dB peak at 440 Hz on the master lifts the whole mix by 6 dB.
    mix.insert(MixerRef::Master, EQ, &[(3, 440.0), (4, 6.0), (5, 1.0)]);
    let boosted = render(&fixture, mix.project());
    near(
        db(rms(&boosted, 0) / rms(&dry, 0)),
        6.0,
        0.15,
        "master EQ lift",
    );
    // Disabled, it is bit-exact bypassed.
    let mut project = mix.project().clone();
    let device = *project.master.inserts.first().unwrap();
    project.devices.get_mut(&device).unwrap().enabled = false;
    assert_eq!(render(&fixture, &project), dry);
    // Master pan balances the mix: hard right silences the left side.
    let mut project = mix.project().clone();
    project.master.settings.pan = 1.0;
    let out = render(&fixture, &project);
    assert!(rms(&out, 0) < 1e-6);
    assert!(rms(&out, 1) > 0.05);
}

/// Two loud tracks that sum well past full scale.
fn loud_mix(ceiling_db: f64, lookahead: f64) -> (Mix, Fixture) {
    let fixture = Fixture::new();
    let mut mix = Mix::new();
    let media = mix.media("loud", 1);
    // 0.9 x 0.707 (centre) x 3 tracks = 1.9 peak.
    for _ in 0..3 {
        mix.track(media, gain(0.0));
    }
    mix.insert(
        MixerRef::Master,
        LIMITER,
        &[(0, ceiling_db), (1, 0.05), (2, lookahead)],
    );
    (mix, fixture)
}

#[test]
fn mix_limiter_never_exceeds_its_ceiling_on_the_master() {
    for (ceiling_db, lookahead) in [(-1.0, 0.002), (-6.0, 0.005), (-0.3, 0.001)] {
        let (mix, fixture) = loud_mix(ceiling_db, lookahead);
        let ceiling = 10.0_f64.powf(ceiling_db / 20.0) as f32;
        let out = render(&fixture, mix.project());
        let top = peak(&out);
        assert!(
            top <= ceiling,
            "peak {top} over {ceiling} ({ceiling_db} dB)"
        );
        assert!(
            top > ceiling * 0.98,
            "the limiter should not undershoot: {top}"
        );
        // Without the limiter the mix is far over.
        let mut project = mix.project().clone();
        let device = project.master.inserts[0];
        project.devices.get_mut(&device).unwrap().enabled = false;
        assert!(peak(&render(&fixture, &project)) > 1.5);
    }
}

#[test]
fn mix_limiter_latency_reaches_the_plan_and_the_render_is_aligned() {
    // A quiet sine through a limiter that never acts: the render equals the dry mix sample for
    // sample, so the lookahead delay was compensated.
    let fixture = Fixture::new();
    let mut mix = Mix::new();
    let media = mix.media("mono", 1);
    mix.track(media, gain(0.0));
    let dry = render(&fixture, mix.project());
    let limiter = mix.insert(MixerRef::Master, LIMITER, &[(0, 0.0), (2, 0.003)]);
    let compiled = compile_project(mix.project(), &fixture.ctx(), RATE, 512).unwrap();
    assert_eq!(compiled.plan.output_latency(), 144, "3 ms at 48 kHz");
    assert!(
        compiled
            .nodes
            .get(ow_engine::NodeKey::Device(limiter))
            .is_some()
    );
    let limited = render(&fixture, mix.project());
    assert_eq!(limited.len(), dry.len());
    let worst = limited
        .iter()
        .zip(&dry)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    assert!(worst < 1e-6, "{worst}");
}

#[test]
fn mix_delay_compensation_lines_up_tracks_around_a_limiter_insert() {
    // The same click on two tracks, one through a 2 ms limiter insert: the click arrives as one
    // click at frame 100, not as two.
    let fixture = Fixture::new();
    let mut mix = Mix::new();
    let media = mix.media("impulse", 1);
    let plain = mix.track(media, gain(0.0));
    let delayed = mix.track(media, gain(0.0));
    mix.insert(MixerRef::Track(delayed), LIMITER, &[(0, 0.0)]);
    let compiled = compile_project(mix.project(), &fixture.ctx(), RATE, 512).unwrap();
    assert_eq!(compiled.plan.output_latency(), 96);
    let out = render(&fixture, mix.project());
    let click = 0.3 * FRAC_1_SQRT_2 as f32;
    let hits: Vec<(usize, f32)> = out
        .chunks(2)
        .enumerate()
        .filter(|(_, frame)| frame[0].abs() > 1e-4)
        .map(|(index, frame)| (index, frame[0]))
        .collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].0, 100);
    assert!((hits[0].1 - 2.0 * click).abs() < 1e-5, "{hits:?}");
    // The strip tap on the plain track is aligned too (no latency of its own: not delayed).
    let alone = render_strip(
        mix.project(),
        &fixture.ctx(),
        MixerRef::Track(plain),
        FRAMES,
    )
    .unwrap();
    assert!((alone[200] - click).abs() < 1e-5);
}

#[test]
fn mix_strip_tap_renders_one_strip_after_its_fader() {
    let fixture = Fixture::new();
    let mut mix = Mix::new();
    let media = mix.media("mono", 1);
    let a = mix.track(
        media,
        StripSettings {
            gain_db: -6.0206,
            pan: 1.0,
            ..StripSettings::default()
        },
    );
    mix.track(media, gain(0.0));
    let out = render_strip(mix.project(), &fixture.ctx(), MixerRef::Track(a), FRAMES).unwrap();
    near(rms(&out, 0), 0.0, 1e-6, "panned away");
    near(rms(&out, 1), SINE_RMS * 0.5, 1e-3, "after the fader");
    let master = render_strip(mix.project(), &fixture.ctx(), MixerRef::Master, FRAMES).unwrap();
    assert_eq!(master, render(&fixture, mix.project()));
    // A strip that does not exist is an error, not a panic.
    assert!(
        render_strip(
            mix.project(),
            &fixture.ctx(),
            MixerRef::Track(TrackId(9_999)),
            100
        )
        .is_err()
    );
}

#[test]
fn mix_rendering_is_deterministic_with_pan_buses_and_a_limiter() {
    let (mut mix, fixture) = loud_mix(-1.0, 0.002);
    let bus = mix.bus(gain(-3.0));
    mix.sends(
        MixerRef::Track(mix.project().tracks[0].id),
        vec![AuxSend {
            target: bus,
            level_db: -6.0,
            pre_fader: true,
        }],
    );
    mix.settings(MixerRef::Track(mix.project().tracks[1].id), panned(-0.4));
    let first = render(&fixture, mix.project());
    assert_eq!(first, render(&fixture, mix.project()));
    assert!(first.iter().all(|s| s.is_finite()));
}
