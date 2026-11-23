//! Track inserts through the compiler and the engine cycle (MVP goal M-G8): built-in effects
//! process their track's audio in chain order, a disabled insert is bit-exact bypassed, an insert
//! the build cannot run is a unity pass-through, and model parameters reach the effect. Audio
//! and MIDI tracks share the path.

use std::f64::consts::TAU;
use std::path::Path;

use ow_engine::{
    CompileContext, MAX_BLOCK, Message, NodeKey, OfflineRenderer, TransportCommand, compile_project,
};
use ow_model::{
    Clip, ClipContent, Device, DeviceId, EditCommand, Editor, MediaItem, Note, Owned, Project,
    Strip, Track, TrackId, TrackKind,
};

const RATE: u32 = 48_000;
const FRAMES: usize = 48_000;
/// Samples ignored at the start of a measurement, so filters have settled.
const SETTLE: usize = 12_000;

const EQ: &str = "builtin.eq";
const COMPRESSOR: &str = "builtin.compressor";
const REVERB: &str = "builtin.reverb";

/// One insert of a test chain: kind, whether it is enabled, and parameter values.
#[derive(Clone)]
struct Insert {
    kind: &'static str,
    enabled: bool,
    params: Vec<(u32, f64)>,
}

fn insert(kind: &'static str, params: &[(u32, f64)]) -> Insert {
    Insert {
        kind,
        enabled: true,
        params: params.to_vec(),
    }
}

fn bypassed(mut insert: Insert) -> Insert {
    insert.enabled = false;
    insert
}

/// +6 dB peak at the source's 440 Hz.
fn eq_boost() -> Insert {
    insert(EQ, &[(3, 440.0), (4, 6.0), (5, 1.0)])
}

/// A strong compressor: it clearly bites a 0.2-amplitude sine.
fn heavy_compressor() -> Insert {
    insert(COMPRESSOR, &[(0, -40.0), (1, 20.0), (2, 0.001), (3, 0.05)])
}

fn wet_reverb() -> Insert {
    insert(REVERB, &[(0, 80.0), (2, 100.0)])
}

fn device_for(id: DeviceId, insert: &Insert) -> Device {
    Device {
        id,
        kind: insert.kind.into(),
        name: insert.kind.into(),
        enabled: insert.enabled,
        params: insert.params.iter().copied().collect(),
        state: vec![],
        sidechain: None,
    }
}

/// A mono 440 Hz sine of amplitude 0.2, two seconds, stored as `audio/tone.wav`.
fn write_tone(root: &Path) {
    std::fs::create_dir_all(root.join("audio")).unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(root.join("audio/tone.wav"), spec).unwrap();
    for i in 0..RATE * 2 {
        let phase = TAU * 440.0 * f64::from(i) / f64::from(RATE);
        writer.write_sample((0.2 * phase.sin()) as f32).unwrap();
    }
    writer.finalize().unwrap();
}

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        write_tone(root.path());
        Self { root }
    }

    fn ctx(&self) -> CompileContext {
        CompileContext::new(self.root.path())
    }
}

/// An audio track playing the tone with `inserts` in order; returns the project and the ids.
fn audio_project(inserts: &[Insert]) -> (Project, Vec<DeviceId>) {
    let mut editor = Editor::new(Project::new("inserts"));
    let media = editor.alloc_id().unwrap();
    editor
        .apply(EditCommand::AddMedia {
            item: MediaItem {
                id: media,
                name: "tone".into(),
                path: "audio/tone.wav".into(),
                hash: "00".into(),
                sample_rate: RATE,
                channels: 1,
                frames: u64::from(RATE) * 2,
            },
        })
        .unwrap();
    let track: TrackId = editor.alloc_id().unwrap();
    let ids: Vec<DeviceId> = inserts.iter().map(|_| editor.alloc_id().unwrap()).collect();
    let clip = editor.alloc_id().unwrap();
    editor
        .apply(EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned {
                item: Track {
                    id: track,
                    name: "Audio".into(),
                    color: 0,
                    kind: TrackKind::Audio,
                    strip: Strip {
                        inserts: ids.clone(),
                        ..Strip::default()
                    },
                    instrument: None,
                },
                clips: vec![Clip {
                    id: clip,
                    track,
                    name: "tone".into(),
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
                devices: ids
                    .iter()
                    .zip(inserts)
                    .map(|(id, insert)| device_for(*id, insert))
                    .collect(),
                lanes: vec![],
            }),
        })
        .unwrap();
    (editor.project().clone(), ids)
}

/// A MIDI track holding one long A4 on the default synth, with `inserts` in order.
fn midi_project(inserts: &[Insert]) -> Project {
    let mut editor = Editor::new(Project::new("midi inserts"));
    let track: TrackId = editor.alloc_id().unwrap();
    let synth: DeviceId = editor.alloc_id().unwrap();
    let ids: Vec<DeviceId> = inserts.iter().map(|_| editor.alloc_id().unwrap()).collect();
    let clip = editor.alloc_id().unwrap();
    let mut devices = vec![device_for(
        synth,
        &Insert {
            kind: "builtin.synth",
            enabled: true,
            params: vec![],
        },
    )];
    devices.extend(ids.iter().zip(inserts).map(|(id, i)| device_for(*id, i)));
    editor
        .apply(EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned {
                item: Track {
                    id: track,
                    name: "Keys".into(),
                    color: 0,
                    kind: TrackKind::Midi,
                    strip: Strip {
                        inserts: ids,
                        ..Strip::default()
                    },
                    instrument: Some(synth),
                },
                clips: vec![Clip {
                    id: clip,
                    track,
                    name: "notes".into(),
                    start: 0.0,
                    length: 8.0,
                    muted: false,
                    content: ClipContent::Midi {
                        notes: vec![Note {
                            start: 0.0,
                            length: 8.0,
                            key: 69,
                            velocity: 100,
                        }],
                        controllers: vec![],
                    },
                }],
                devices,
                lanes: vec![],
            }),
        })
        .unwrap();
    editor.project().clone()
}

/// Both channels of the project's first `FRAMES` frames, interleaved.
fn render(fixture: &Fixture, project: &Project) -> Vec<f32> {
    let compiled = compile_project(project, &fixture.ctx(), RATE, MAX_BLOCK).unwrap();
    let mut renderer = OfflineRenderer::new(compiled.plan, 2);
    assert!(renderer.link().swap_timeline(compiled.timeline).is_ok());
    assert!(renderer.send(Message::Transport(TransportCommand::Play)));
    renderer.render(FRAMES, 512)
}

fn rms(signal: &[f32]) -> f64 {
    let left = signal
        .chunks(2)
        .skip(SETTLE)
        .map(|frame| f64::from(frame[0]));
    let (sum, count) = left.fold((0.0, 0), |(s, n), x| (s + x * x, n + 1));
    (sum / f64::from(count)).sqrt()
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.log10()
}

#[test]
fn inserts_apply_in_chain_order() {
    let fixture = Fixture::new();
    let (dry, _) = audio_project(&[]);
    let dry = render(&fixture, &dry);
    assert!(
        (rms(&dry) - 0.2 / 2.0_f64.sqrt()).abs() < 1e-3,
        "the source plays"
    );

    // The EQ's +6 dB peak at 440 Hz lifts the level by 6 dB, to within the 0.1 dB band tolerance.
    let (boosted, _) = audio_project(&[eq_boost()]);
    let boosted = render(&fixture, &boosted);
    let lift = db(rms(&boosted) / rms(&dry));
    assert!((lift - 6.0).abs() < 0.1, "EQ lift {lift} dB");

    // The compressor alone pulls the level well down; its effect depends on what feeds it, so
    // EQ-then-compressor and compressor-then-EQ differ audibly.
    let (squeezed, _) = audio_project(&[heavy_compressor()]);
    let squeezed = render(&fixture, &squeezed);
    assert!(db(rms(&squeezed) / rms(&dry)) < -6.0);

    let (eq_first, _) = audio_project(&[eq_boost(), heavy_compressor()]);
    let (comp_first, _) = audio_project(&[heavy_compressor(), eq_boost()]);
    let (eq_first, comp_first) = (render(&fixture, &eq_first), render(&fixture, &comp_first));
    assert!(eq_first.iter().chain(&comp_first).all(|s| s.is_finite()));
    let difference = db(rms(&eq_first) / rms(&comp_first));
    assert!(
        difference.abs() > 0.5,
        "the order matters: {difference} dB apart"
    );
    // Chain order is the strip's order: the same two devices listed the other way round.
    let (mut project, ids) = audio_project(&[eq_boost(), heavy_compressor()]);
    project.tracks[0].strip.inserts = vec![ids[1], ids[0]];
    assert_eq!(render(&fixture, &project), comp_first);
}

#[test]
fn inserts_bypass_is_bit_exact_and_enabling_changes_the_sound() {
    let fixture = Fixture::new();
    let (without, _) = audio_project(&[]);
    let without = render(&fixture, &without);
    for effect in [eq_boost(), heavy_compressor(), wet_reverb()] {
        let kind = effect.kind;
        let (off, ids) = audio_project(&[bypassed(effect.clone())]);
        let compiled = compile_project(&off, &fixture.ctx(), RATE, MAX_BLOCK).unwrap();
        assert!(
            compiled.nodes.get(NodeKey::Device(ids[0])).is_some(),
            "{kind}: a bypassed insert keeps its node"
        );
        assert_eq!(render(&fixture, &off), without, "{kind} bypassed");

        let (on, _) = audio_project(&[effect]);
        assert_ne!(render(&fixture, &on), without, "{kind} enabled");
    }

    // Bypassing the middle of three leaves the other two in place.
    let (all, _) = audio_project(&[eq_boost(), heavy_compressor(), wet_reverb()]);
    let (middle_off, _) = audio_project(&[eq_boost(), bypassed(heavy_compressor()), wet_reverb()]);
    let (middle_gone, _) = audio_project(&[eq_boost(), wet_reverb()]);
    assert_eq!(
        render(&fixture, &middle_off),
        render(&fixture, &middle_gone)
    );
    assert_ne!(render(&fixture, &middle_off), render(&fixture, &all));
}

#[test]
fn inserts_of_an_unusable_kind_pass_the_signal_through_unchanged() {
    let fixture = Fixture::new();
    let (without, _) = audio_project(&[]);
    let without = render(&fixture, &without);
    for kind in ["vst3:something", "builtin.nothing", "builtin.synth"] {
        let (project, _) = audio_project(&[insert(kind, &[])]);
        assert_eq!(render(&fixture, &project), without, "{kind}");
    }
}

#[test]
fn inserts_take_their_parameters_from_the_model() {
    let fixture = Fixture::new();
    let level = |gain: f64| {
        let (project, _) = audio_project(&[insert(EQ, &[(3, 440.0), (4, gain), (5, 1.0)])]);
        rms(&render(&fixture, &project))
    };
    let flat = level(0.0);
    assert!((db(level(-9.0) / flat) + 9.0).abs() < 0.1);
    // A value outside the descriptor's range is clamped (the peak gain tops out at +18 dB).
    assert!((db(level(60.0) / flat) - 18.0).abs() < 0.1);

    // The reverb adds a tail after the clip's source ends; without it the output stops.
    let (project, _) = audio_project(&[wet_reverb()]);
    let out = render(&fixture, &project);
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(
        out.chunks(2).skip(SETTLE).any(|f| f[0] != f[1]),
        "stereo spread"
    );
}

#[test]
fn inserts_work_on_midi_tracks_after_the_instrument() {
    let fixture = Fixture::new();
    let without = render(&fixture, &midi_project(&[]));
    assert!(rms(&without) > 0.01, "the synth sounds");
    let boosted = render(&fixture, &midi_project(&[eq_boost()]));
    let lift = db(rms(&boosted) / rms(&without));
    // The default saw has harmonics the 440 Hz peak does not lift, so the gain is below 6 dB.
    assert!((1.0..6.0).contains(&lift), "EQ lift {lift} dB");
    assert_eq!(
        render(&fixture, &midi_project(&[bypassed(eq_boost())])),
        without
    );
    let reverbed = render(&fixture, &midi_project(&[wet_reverb()]));
    assert_ne!(reverbed, without);
    assert!(reverbed.iter().all(|s| s.is_finite()));
}

/// A stereo source of a steady 0.2 that puts a NaN and an infinity (in different channels and
/// blocks) into the stream, then goes back to finite samples.
struct Poison {
    block: usize,
}

impl ow_engine::Node for Poison {
    fn name(&self) -> &'static str {
        "poison"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        2
    }
    fn process(&mut self, ctx: &mut ow_engine::ProcessContext<'_>) {
        for channel in 0..2 {
            ctx.outputs.channel_mut(channel).fill(0.2);
        }
        match self.block {
            2 => ctx.outputs.channel_mut(0)[5] = f32::NAN,
            3 => ctx.outputs.channel_mut(1)[7] = f32::INFINITY,
            4 => ctx.outputs.channel_mut(0)[1] = f32::NEG_INFINITY,
            _ => {}
        }
        self.block += 1;
    }
}

#[test]
fn inserts_survive_non_finite_input_and_recover_on_the_next_finite_block() {
    for info in ow_dsp::all().iter().filter(|info| !info.instrument) {
        let kind = info.kind;
        let mut graph = ow_engine::Graph::new();
        let source = graph.add_node(Box::new(Poison { block: 0 }));
        let effect = graph.add_node(Box::new(ow_engine::DeviceNode::effect(
            info,
            info.create(RATE as f32, MAX_BLOCK),
        )));
        let master = graph.add_node(Box::new(ow_engine::MasterNode::new()));
        for channel in 0..2 {
            graph.connect(source, channel, effect, channel).unwrap();
            graph.connect(effect, channel, master, channel).unwrap();
        }
        graph.set_master(master).unwrap();
        let mut renderer = OfflineRenderer::new(graph.compile(RATE, MAX_BLOCK).unwrap(), 2);
        renderer.send(Message::Transport(TransportCommand::Play));
        let out = renderer.render(64 * 20, 64);
        assert!(
            out.iter().all(|s| s.is_finite()),
            "{kind}: output stays finite"
        );
        let tail = &out[out.len() - 128..];
        assert!(
            tail.iter().any(|s| s.abs() > 1.0e-3),
            "{kind}: sound continues after the bad samples"
        );
    }
}
