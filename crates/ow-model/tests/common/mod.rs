//! Shared helpers: a deterministic reference project and a generator that turns random numbers
//! into commands valid (or deliberately invalid) for the project's current state.

#![allow(dead_code, clippy::manual_is_multiple_of)]

use std::collections::BTreeMap;

use ow_model::time::{TempoMap, TempoPoint, TimeSignature, TimeSignatureMap};
use ow_model::*;
use proptest::prelude::*;

/// One random step of a command sequence.
#[derive(Debug, Clone, Copy)]
pub struct Op {
    pub kind: u8,
    pub a: u16,
    pub b: u16,
    pub c: u16,
}

pub fn op() -> impl Strategy<Value = Op> {
    (0_u8..40, any::<u16>(), any::<u16>(), any::<u16>()).prop_map(|(kind, a, b, c)| Op {
        kind,
        a,
        b,
        c,
    })
}

pub fn ops(max: usize) -> impl Strategy<Value = Vec<Op>> {
    proptest::collection::vec(op(), 1..max)
}

fn pick<T: Copy>(items: &[T], seed: u16) -> Option<T> {
    if items.is_empty() {
        None
    } else {
        Some(items[usize::from(seed) % items.len()])
    }
}

fn beats(seed: u16) -> f64 {
    f64::from(seed % 256) * 0.25
}

fn len_beats(seed: u16) -> f64 {
    f64::from(seed % 64 + 1) * 0.25
}

fn strip_settings(seed: u16) -> StripSettings {
    StripSettings {
        gain_db: -f64::from(seed % 40),
        pan: f64::from(i32::from(seed % 21) - 10) / 10.0,
        mute: seed % 7 == 0,
        solo: seed % 11 == 0,
        solo_safe: seed % 13 == 0,
        invert_phase: seed % 5 == 0,
        output: Output::Master,
    }
}

fn device(ed: &mut Editor, seed: u16) -> Device {
    let id: DeviceId = ed.alloc_id().unwrap();
    let mut params = BTreeMap::new();
    params.insert(u32::from(seed % 8), f64::from(seed) / 100.0);
    Device {
        id,
        kind: format!("builtin.dev{}", seed % 5),
        name: format!("Device {seed}"),
        enabled: seed % 3 != 0,
        params,
        state: vec![StateChunk {
            format: "builtin".into(),
            data: format!("chunk{seed}"),
        }],
        sidechain: None,
    }
}

fn mixers(p: &Project) -> Vec<MixerRef> {
    let mut all = vec![MixerRef::Master];
    all.extend(p.tracks.iter().map(|t| MixerRef::Track(t.id)));
    all.extend(p.buses.iter().map(|b| MixerRef::Bus(b.id)));
    all
}

fn note(seed: u16) -> Note {
    Note {
        start: beats(seed),
        length: len_beats(seed / 3),
        key: (seed % 128) as u8,
        velocity: (seed % 127 + 1) as u8,
    }
}

fn media_item(ed: &mut Editor, seed: u16) -> MediaItem {
    let id: MediaId = ed.alloc_id().unwrap();
    MediaItem {
        id,
        name: format!("Take {seed}"),
        path: format!("audio/{seed:04x}.wav"),
        hash: format!("{seed:016x}"),
        sample_rate: 48_000,
        channels: 1 + (seed % 2),
        frames: u64::from(seed) * 100,
    }
}

fn clip_for(ed: &mut Editor, track: &Track, op: Op) -> Option<Clip> {
    let p = ed.project();
    let content = match track.kind {
        TrackKind::Audio => {
            let media: Vec<MediaId> = p.media_pool.keys().copied().collect();
            ClipContent::Audio {
                media: pick(&media, op.b)?,
                source_offset: u64::from(op.c),
                gain_db: -f64::from(op.c % 12),
                fade_in: 0.0,
                fade_out: f64::from(op.c % 4) * 0.25,
            }
        }
        TrackKind::Midi => {
            let patterns: Vec<PatternId> = p.patterns.keys().copied().collect();
            if op.b % 2 == 0
                && let Some(pattern) = pick(&patterns, op.c)
            {
                ClipContent::Pattern { pattern }
            } else {
                ClipContent::Midi {
                    notes: vec![note(op.b), note(op.c)],
                    controllers: vec![Controller {
                        beat: beats(op.c),
                        controller: (op.b % 128) as u8,
                        value: (op.c % 128) as u8,
                    }],
                }
            }
        }
    };
    let id: ClipId = ed.alloc_id().unwrap();
    Some(Clip {
        id,
        track: track.id,
        name: format!("Clip {}", op.a),
        start: beats(op.b),
        length: len_beats(op.c),
        muted: op.a % 9 == 0,
        content,
        take: None,
    })
}

/// Builds the command `op` stands for against the editor's current project, or `None` when the
/// project has nothing for it to act on. Some commands are invalid on purpose (cycles,
/// occupied slots); applying them must fail and change nothing.
pub fn make_command(ed: &mut Editor, op: Op) -> Option<EditCommand> {
    let p = ed.project().clone();
    let tracks: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
    let buses: Vec<BusId> = p.buses.iter().map(|b| b.id).collect();
    let devices: Vec<DeviceId> = p.devices.keys().copied().collect();
    let clips: Vec<ClipId> = p.clips.keys().copied().collect();
    let patterns: Vec<PatternId> = p.patterns.keys().copied().collect();
    let media: Vec<MediaId> = p.media_pool.keys().copied().collect();
    let lanes: Vec<LaneId> = p.automation.keys().copied().collect();
    let markers: Vec<MarkerId> = p.markers.keys().copied().collect();
    let strips = mixers(&p);
    Some(match op.kind {
        0 => {
            let id: TrackId = ed.alloc_id().unwrap();
            let kind = if op.b % 2 == 0 {
                TrackKind::Audio
            } else {
                TrackKind::Midi
            };
            let mut track = Track {
                id,
                name: format!("Track {}", op.a),
                color: u32::from(op.c) * 100,
                kind,
                strip: Strip {
                    settings: strip_settings(op.c),
                    sends: vec![],
                    inserts: vec![],
                },
                instrument: None,
                input: None,
                monitor: Default::default(),
            };
            if let Some(bus) = pick(&buses, op.c) {
                track.strip.settings.output = Output::Bus(bus);
            }
            EditCommand::InsertTrack {
                index: usize::from(op.a) % (p.tracks.len() + 1),
                bundle: Box::new(Owned::new(track)),
            }
        }
        1 => EditCommand::RemoveTrack {
            id: pick(&tracks, op.a)?,
        },
        2 => EditCommand::MoveTrack {
            id: pick(&tracks, op.a)?,
            to: usize::from(op.b) % p.tracks.len().max(1),
        },
        3 => {
            let id: BusId = ed.alloc_id().unwrap();
            let mut strip = Strip {
                settings: strip_settings(op.c),
                ..Strip::default()
            };
            // Possibly route into another bus (valid) or, later, create cycles via SetStripSettings.
            if op.b % 3 == 0
                && let Some(bus) = pick(&buses, op.b)
            {
                strip.settings.output = Output::Bus(bus);
            }
            EditCommand::InsertBus {
                index: usize::from(op.a) % (p.buses.len() + 1),
                bundle: Box::new(Owned::new(Bus {
                    id,
                    name: format!("Bus {}", op.a),
                    color: u32::from(op.b),
                    strip,
                })),
            }
        }
        4 => EditCommand::RemoveBus {
            id: pick(&buses, op.a)?,
        },
        5 => EditCommand::MoveBus {
            id: pick(&buses, op.a)?,
            to: usize::from(op.b) % p.buses.len().max(1),
        },
        6 => EditCommand::SetInfo {
            target: pick(&strips, op.a)?,
            name: format!("Renamed {}", op.b),
            color: u32::from(op.c),
        },
        7 => {
            let target = pick(&strips, op.a)?;
            let mut settings = strip_settings(op.b);
            // Sometimes a bus output, which may form a cycle (then it must be refused).
            if op.c % 2 == 0
                && let Some(bus) = pick(&buses, op.c)
            {
                settings.output = Output::Bus(bus);
            }
            EditCommand::SetStripSettings { target, settings }
        }
        8 => {
            let target = pick(&strips, op.a)?;
            let sends = pick(&buses, op.b)
                .map(|target| {
                    vec![AuxSend {
                        target,
                        level_db: -f64::from(op.c % 30),
                        pre_fader: op.c % 2 == 0,
                    }]
                })
                .unwrap_or_default();
            EditCommand::SetSends { target, sends }
        }
        9 => {
            let strip = pick(&strips, op.a)?;
            let len = p.strip(strip)?.inserts.len();
            let device = device(ed, op.b);
            let lanes = if op.c % 2 == 0 {
                let id: LaneId = ed.alloc_id().unwrap();
                vec![AutomationLane {
                    id,
                    target: AutomationTarget::Device {
                        device: device.id,
                        param: 1,
                    },
                    points: vec![
                        AutomationPoint {
                            beat: 0.0,
                            value: 0.0,
                            curve: 0.0,
                        },
                        AutomationPoint {
                            beat: beats(op.c),
                            value: 1.0,
                            curve: 0.5,
                        },
                    ],
                    enabled: true,
                }]
            } else {
                vec![]
            };
            EditCommand::InsertDevice {
                slot: DeviceSlot::Insert {
                    strip,
                    index: usize::from(op.c) % (len + 1),
                },
                bundle: Box::new(DeviceBundle { device, lanes }),
            }
        }
        10 => {
            let track = pick(&tracks, op.a)?;
            EditCommand::InsertDevice {
                slot: DeviceSlot::Instrument { track },
                bundle: Box::new(DeviceBundle {
                    device: device(ed, op.b),
                    lanes: vec![],
                }),
            }
        }
        11 => EditCommand::RemoveDevice {
            id: pick(&devices, op.a)?,
        },
        12 => EditCommand::SetDeviceParam {
            device: pick(&devices, op.a)?,
            param: u32::from(op.b % 6),
            value: if op.c % 4 == 0 {
                None
            } else {
                Some(f64::from(op.c) / 7.0)
            },
        },
        13 => EditCommand::SetDeviceState {
            device: pick(&devices, op.a)?,
            state: vec![StateChunk {
                format: "clap".into(),
                data: format!("state{}", op.b),
            }],
        },
        14 => EditCommand::SetDeviceEnabled {
            device: pick(&devices, op.a)?,
            enabled: op.b % 2 == 0,
        },
        15 => EditCommand::SetDeviceSidechain {
            device: pick(&devices, op.a)?,
            source: if op.c % 3 == 0 {
                None
            } else {
                pick(&strips, op.b)
            },
        },
        16 => {
            let track = p.track(pick(&tracks, op.a)?)?;
            EditCommand::AddClip {
                clip: Box::new(clip_for(ed, track, op)?),
            }
        }
        17 => EditCommand::RemoveClip {
            id: pick(&clips, op.a)?,
        },
        18 => {
            let mut clip = p.clips.get(&pick(&clips, op.a)?)?.clone();
            clip.start = beats(op.b);
            clip.length = len_beats(op.c);
            // Occasionally retarget to another track (valid only when the kind matches).
            if op.c % 5 == 0
                && let Some(track) = pick(&tracks, op.b)
            {
                clip.track = track;
            }
            EditCommand::ReplaceClip {
                clip: Box::new(clip),
            }
        }
        19 => {
            let id: PatternId = ed.alloc_id().unwrap();
            EditCommand::AddPattern {
                pattern: Pattern {
                    id,
                    name: format!("Pattern {}", op.a),
                    color: u32::from(op.b),
                    length: len_beats(op.c) * 4.0,
                    notes: vec![note(op.a), note(op.b)],
                },
            }
        }
        20 => EditCommand::RemovePattern {
            id: pick(&patterns, op.a)?,
        },
        21 => {
            let mut pattern = p.patterns.get(&pick(&patterns, op.a)?)?.clone();
            pattern.notes.push(note(op.b));
            EditCommand::ReplacePattern { pattern }
        }
        22 => {
            let instance: Vec<ClipId> = p
                .clips
                .values()
                .filter(|c| matches!(c.content, ClipContent::Pattern { .. }))
                .map(|c| c.id)
                .collect();
            EditCommand::MakeUnique {
                clip: pick(&instance, op.a)?,
                pattern: ed.alloc_id().unwrap(),
            }
        }
        23 => EditCommand::AddMedia {
            item: media_item(ed, op.a),
        },
        24 => EditCommand::RemoveMedia {
            id: pick(&media, op.a)?,
        },
        25 => {
            let id: LaneId = ed.alloc_id().unwrap();
            let target = if op.b % 2 == 0 {
                AutomationTarget::Device {
                    device: pick(&devices, op.a)?,
                    param: u32::from(op.c % 4),
                }
            } else {
                AutomationTarget::Strip {
                    strip: pick(&strips, op.a)?,
                    param: if op.c % 2 == 0 {
                        StripParam::Gain
                    } else {
                        StripParam::Pan
                    },
                }
            };
            EditCommand::AddLane {
                lane: AutomationLane {
                    id,
                    target,
                    points: vec![AutomationPoint {
                        beat: beats(op.b),
                        value: f64::from(op.c),
                        curve: 0.0,
                    }],
                    enabled: op.c % 2 == 1,
                },
            }
        }
        26 => EditCommand::RemoveLane {
            id: pick(&lanes, op.a)?,
        },
        27 => {
            let mut lane = p.automation.get(&pick(&lanes, op.a)?)?.clone();
            lane.points.push(AutomationPoint {
                beat: lane.points.last().map_or(0.0, |q| q.beat) + len_beats(op.b),
                value: f64::from(op.c),
                curve: -0.5,
            });
            EditCommand::ReplaceLane { lane }
        }
        28 => EditCommand::AddMarker {
            marker: Marker {
                id: ed.alloc_id().unwrap(),
                name: format!("Marker {}", op.a),
                beat: beats(op.b),
                color: u32::from(op.c),
            },
        },
        29 => EditCommand::RemoveMarker {
            id: pick(&markers, op.a)?,
        },
        30 => {
            let mut marker = p.markers.get(&pick(&markers, op.a)?)?.clone();
            marker.beat = beats(op.b);
            EditCommand::ReplaceMarker { marker }
        }
        31 => EditCommand::SetName {
            name: format!("Project {}", op.a),
        },
        32 => EditCommand::SetTempoMap {
            tempo_map: TempoMap::new(vec![
                TempoPoint::constant(0.0, 60.0 + f64::from(op.a % 100)),
                TempoPoint::ramp(8.0, 100.0 + f64::from(op.b % 100)),
                TempoPoint::constant(16.0 + f64::from(op.c % 8), 90.0),
            ])
            .ok()?,
        },
        33 => EditCommand::SetTimeSignatureMap {
            map: TimeSignatureMap::new(vec![
                (0, TimeSignature::new(4, 4).ok()?),
                (
                    1 + u32::from(op.a % 8),
                    TimeSignature::new(1 + u32::from(op.b % 12), 1 << (op.c % 5)).ok()?,
                ),
            ])
            .ok()?,
        },
        34 => EditCommand::SetSettings {
            settings: ProjectSettings {
                sample_rate: 44_100 + u32::from(op.a % 3) * 1000,
                loop_range: if op.b % 2 == 0 {
                    Some(BeatRange {
                        start: beats(op.b),
                        end: beats(op.b) + len_beats(op.c),
                    })
                } else {
                    None
                },
                count_in_bars: (op.c % 5) as u8,
                punch_enabled: false,
                punch_range: None,
            },
        },
        35 => {
            // A whole track with a device, a lane and a clip, inserted in one command.
            let id: TrackId = ed.alloc_id().unwrap();
            let dev = device(ed, op.b);
            let lane_id: LaneId = ed.alloc_id().unwrap();
            let mut track = Track {
                id,
                name: "Bundle".into(),
                color: 7,
                kind: TrackKind::Midi,
                strip: Strip::default(),
                instrument: Some(dev.id),
                input: None,
                monitor: Default::default(),
            };
            track.strip.sends = pick(&buses, op.c)
                .map(|target| {
                    vec![AuxSend {
                        target,
                        level_db: -6.0,
                        pre_fader: false,
                    }]
                })
                .unwrap_or_default();
            let clip_id: ClipId = ed.alloc_id().unwrap();
            EditCommand::InsertTrack {
                index: usize::from(op.a) % (p.tracks.len() + 1),
                bundle: Box::new(Owned {
                    item: track,
                    clips: vec![Clip {
                        id: clip_id,
                        track: id,
                        name: "In bundle".into(),
                        start: 0.0,
                        length: 4.0,
                        muted: false,
                        content: ClipContent::Midi {
                            notes: vec![note(op.c)],
                            controllers: vec![],
                        },
                        take: None,
                    }],
                    lanes: vec![AutomationLane {
                        id: lane_id,
                        target: AutomationTarget::Device {
                            device: dev.id,
                            param: 0,
                        },
                        points: vec![],
                        enabled: true,
                    }],
                    devices: vec![dev],
                }),
            }
        }
        36 => EditCommand::Batch {
            commands: vec![
                EditCommand::AddMarker {
                    marker: Marker {
                        id: ed.alloc_id().unwrap(),
                        name: "batch".into(),
                        beat: beats(op.a),
                        color: 1,
                    },
                },
                // May fail (no such clip): the whole batch must then roll back.
                EditCommand::RemoveClip {
                    id: pick(&clips, op.b).unwrap_or(ClipId(u64::MAX)),
                },
                EditCommand::SetName {
                    name: format!("Batch {}", op.c),
                },
            ],
        },
        // Deliberately invalid: out-of-range values and dangling ids.
        37 => EditCommand::AddClip {
            clip: Box::new(Clip {
                id: ed.alloc_id().unwrap(),
                track: pick(&tracks, op.a).unwrap_or(TrackId(u64::MAX)),
                name: "bad".into(),
                start: f64::NAN,
                length: 1.0,
                muted: false,
                content: ClipContent::Midi {
                    notes: vec![],
                    controllers: vec![],
                },
                take: None,
            }),
        },
        38 => EditCommand::SetStripSettings {
            target: pick(&strips, op.a)?,
            settings: StripSettings {
                gain_db: 1e9,
                ..StripSettings::default()
            },
        },
        _ => EditCommand::RemoveTrack {
            id: TrackId(u64::from(op.a) + 1_000_000),
        },
    })
}

/// A project that uses every entity kind, with fixed ids (the committed golden file).
pub fn reference_project() -> Project {
    let mut ed = Editor::new(Project::new("Reference song"));
    let apply = |ed: &mut Editor, command: EditCommand| ed.apply(command).expect("reference");

    apply(
        &mut ed,
        EditCommand::SetTempoMap {
            tempo_map: TempoMap::new(vec![
                TempoPoint::constant(0.0, 120.0),
                TempoPoint::ramp(16.0, 120.0),
                TempoPoint::constant(32.0, 90.0),
            ])
            .unwrap(),
        },
    );
    apply(
        &mut ed,
        EditCommand::SetTimeSignatureMap {
            map: TimeSignatureMap::new(vec![
                (0, TimeSignature::new(4, 4).unwrap()),
                (8, TimeSignature::new(6, 8).unwrap()),
            ])
            .unwrap(),
        },
    );
    apply(
        &mut ed,
        EditCommand::SetSettings {
            settings: ProjectSettings {
                sample_rate: 48_000,
                loop_range: Some(BeatRange {
                    start: 4.0,
                    end: 12.0,
                }),
                count_in_bars: 1,
                punch_enabled: false,
                punch_range: None,
            },
        },
    );
    let bus: BusId = ed.alloc_id().unwrap();
    let fx: DeviceId = ed.alloc_id().unwrap();
    apply(
        &mut ed,
        EditCommand::InsertBus {
            index: 0,
            bundle: Box::new(Owned {
                item: Bus {
                    id: bus,
                    name: "Drums".into(),
                    color: 0xFF8800,
                    strip: Strip {
                        settings: StripSettings::default(),
                        sends: vec![],
                        inserts: vec![fx],
                    },
                },
                clips: vec![],
                devices: vec![Device {
                    id: fx,
                    kind: "builtin.compressor".into(),
                    name: "Compressor".into(),
                    enabled: true,
                    params: BTreeMap::from([(0, -18.0), (1, 4.0)]),
                    state: vec![],
                    sidechain: Some(MixerRef::Master),
                }],
                lanes: vec![],
            }),
        },
    );
    let media: MediaId = ed.alloc_id().unwrap();
    apply(
        &mut ed,
        EditCommand::AddMedia {
            item: MediaItem {
                id: media,
                name: "Vocal take".into(),
                path: "audio/3fa9c0de.wav".into(),
                hash: "3fa9c0de".into(),
                sample_rate: 48_000,
                channels: 2,
                frames: 480_000,
            },
        },
    );
    let audio: TrackId = ed.alloc_id().unwrap();
    apply(
        &mut ed,
        EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned::new(Track {
                id: audio,
                name: "Vocals".into(),
                color: 0x3366FF,
                kind: TrackKind::Audio,
                strip: Strip {
                    settings: StripSettings {
                        gain_db: -3.5,
                        pan: -0.25,
                        ..StripSettings::default()
                    },
                    sends: vec![AuxSend {
                        target: bus,
                        level_db: -12.0,
                        pre_fader: false,
                    }],
                    inserts: vec![],
                },
                instrument: None,
                input: None,
                monitor: Default::default(),
            })),
        },
    );
    let clip: ClipId = ed.alloc_id().unwrap();
    apply(
        &mut ed,
        EditCommand::AddClip {
            clip: Box::new(Clip {
                id: clip,
                track: audio,
                name: "Verse".into(),
                start: 4.0,
                length: 8.0,
                muted: false,
                content: ClipContent::Audio {
                    media,
                    source_offset: 1_000,
                    gain_db: -1.5,
                    fade_in: 0.25,
                    fade_out: 0.5,
                },
                take: None,
            }),
        },
    );
    let midi: TrackId = ed.alloc_id().unwrap();
    let synth: DeviceId = ed.alloc_id().unwrap();
    let lane: LaneId = ed.alloc_id().unwrap();
    apply(
        &mut ed,
        EditCommand::InsertTrack {
            index: 1,
            bundle: Box::new(Owned {
                item: Track {
                    id: midi,
                    name: "Keys".into(),
                    color: 0x00CC66,
                    kind: TrackKind::Midi,
                    strip: Strip {
                        settings: StripSettings {
                            output: Output::Bus(bus),
                            ..StripSettings::default()
                        },
                        sends: vec![],
                        inserts: vec![],
                    },
                    instrument: Some(synth),
                    input: None,
                    monitor: Default::default(),
                },
                clips: vec![],
                devices: vec![Device {
                    id: synth,
                    kind: "builtin.subtractive".into(),
                    name: "Synth".into(),
                    enabled: true,
                    params: BTreeMap::from([(2, 0.5)]),
                    state: vec![StateChunk {
                        format: "builtin".into(),
                        data: "AAEC".into(),
                    }],
                    sidechain: None,
                }],
                lanes: vec![AutomationLane {
                    id: lane,
                    target: AutomationTarget::Device {
                        device: synth,
                        param: 2,
                    },
                    points: vec![
                        AutomationPoint {
                            beat: 0.0,
                            value: 0.1,
                            curve: 0.0,
                        },
                        AutomationPoint {
                            beat: 8.0,
                            value: 0.9,
                            curve: -0.5,
                        },
                    ],
                    enabled: true,
                }],
            }),
        },
    );
    let pattern: PatternId = ed.alloc_id().unwrap();
    apply(
        &mut ed,
        EditCommand::AddPattern {
            pattern: Pattern {
                id: pattern,
                name: "Chords".into(),
                color: 0xAA00FF,
                length: 4.0,
                notes: vec![
                    Note {
                        start: 0.0,
                        length: 1.0,
                        key: 60,
                        velocity: 100,
                    },
                    Note {
                        start: 1.0,
                        length: 0.5,
                        key: 64,
                        velocity: 90,
                    },
                ],
            },
        },
    );
    let instance: ClipId = ed.alloc_id().unwrap();
    apply(
        &mut ed,
        EditCommand::AddClip {
            clip: Box::new(Clip {
                id: instance,
                track: midi,
                name: "Chords".into(),
                start: 0.0,
                length: 16.0,
                muted: false,
                content: ClipContent::Pattern { pattern },
                take: None,
            }),
        },
    );
    let midi_clip: ClipId = ed.alloc_id().unwrap();
    apply(
        &mut ed,
        EditCommand::AddClip {
            clip: Box::new(Clip {
                id: midi_clip,
                track: midi,
                name: "Lead".into(),
                start: 16.0,
                length: 4.0,
                muted: true,
                content: ClipContent::Midi {
                    notes: vec![Note {
                        start: 0.5,
                        length: 0.25,
                        key: 72,
                        velocity: 127,
                    }],
                    controllers: vec![Controller {
                        beat: 1.0,
                        controller: 1,
                        value: 64,
                    }],
                },
                take: None,
            }),
        },
    );
    let strip_lane: LaneId = ed.alloc_id().unwrap();
    apply(
        &mut ed,
        EditCommand::AddLane {
            lane: AutomationLane {
                id: strip_lane,
                target: AutomationTarget::Strip {
                    strip: MixerRef::Track(audio),
                    param: StripParam::Gain,
                },
                points: vec![AutomationPoint {
                    beat: 4.0,
                    value: -6.0,
                    curve: 0.0,
                }],
                enabled: false,
            },
        },
    );
    let marker: MarkerId = ed.alloc_id().unwrap();
    apply(
        &mut ed,
        EditCommand::AddMarker {
            marker: Marker {
                id: marker,
                name: "Chorus".into(),
                beat: 32.0,
                color: 0xFF0000,
            },
        },
    );
    let project = ed.into_project();
    project.validate().expect("reference project is valid");
    project
}
