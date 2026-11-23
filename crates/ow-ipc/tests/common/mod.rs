//! Shared by the mirror tests and the fixture generator: abstract actions that are turned into
//! concrete UI commands against the session project, so random sequences hit real entities (and
//! sometimes deliberately invalid ones).

#![allow(dead_code)]

use ow_ipc::{EditCommand, MixerRef, ModelPatch, Session, UiCommand};
use ow_model::time::{TimeSignature, TimeSignatureMap};
use ow_model::{
    AutomationLane, AutomationPoint, AutomationTarget, AuxSend, BeatRange, Bus, Clip, ClipContent,
    Device, DeviceBundle, DeviceSlot, Marker, MediaItem, Note, Output, Owned, Pattern, StateChunk,
    Strip, StripParam, StripSettings, Track, TrackKind,
};
use proptest::prelude::*;

#[derive(Debug, Clone)]
pub enum Action {
    AddTrack {
        audio: bool,
        at: usize,
    },
    RemoveTrack(usize),
    MoveTrack(usize, usize),
    AddBus,
    RemoveBus(usize),
    SetStrip {
        pick: usize,
        gain: i16,
        mute: bool,
        route_bus: Option<usize>,
    },
    SetInfo {
        pick: usize,
        name: u8,
    },
    AddClip {
        track: usize,
        start: u16,
    },
    ReplaceClip {
        pick: usize,
        start: u16,
    },
    RemoveClip(usize),
    AddPattern,
    AddMarker(u16),
    RemoveMarker(usize),
    AddLane(usize),
    SetName(u8),
    SetTempo(u16),
    Undo,
    Redo,
    Begin,
    End,
    Cancel,
    Alloc(u8),
    /// A command the model must refuse (unknown id): no patch results.
    Bogus,
    /// A track that owns a device, a clip and automation lanes (removing and undoing it moves
    /// all of them).
    AddTrackBundle {
        audio: bool,
        at: usize,
    },
    AddDevice {
        pick: usize,
        index: usize,
    },
    RemoveDevice(usize),
    SetDeviceParam {
        pick: usize,
        param: u8,
        value: Option<i8>,
    },
    SetDeviceState {
        pick: usize,
        n: u8,
    },
    SetDeviceEnabled {
        pick: usize,
        enabled: bool,
    },
    SetDeviceSidechain {
        pick: usize,
        source: Option<usize>,
    },
    AddMedia,
    RemoveMedia(usize),
    ReplaceMedia(usize),
    AddAudioClip {
        track: usize,
        media: usize,
    },
    SetSettings {
        loop_range: Option<(u8, u8)>,
        count_in: u8,
    },
    SetTimeSignature {
        numerator: u8,
        shift: u8,
    },
    SetSends {
        pick: usize,
        bus: usize,
        level: i8,
    },
    MoveBus(usize, usize),
    ReplacePattern {
        pick: usize,
        length: u8,
    },
    AddPatternClip {
        track: usize,
        pattern: usize,
    },
    MakeUnique(usize),
    ReplaceLane {
        pick: usize,
        value: i8,
    },
    ReplaceMarker {
        pick: usize,
        beat: u16,
    },
    /// Two commands applied together.
    Batch,
    /// A batch whose second command fails: nothing may change.
    FailingBatch,
}

pub fn action() -> impl Strategy<Value = Action> {
    prop_oneof![
        3 => (any::<bool>(), 0..6_usize).prop_map(|(audio, at)| Action::AddTrack { audio, at }),
        1 => (0..8_usize).prop_map(Action::RemoveTrack),
        2 => (0..8_usize, 0..8_usize).prop_map(|(a, b)| Action::MoveTrack(a, b)),
        1 => Just(Action::AddBus),
        1 => (0..4_usize).prop_map(Action::RemoveBus),
        3 => (0..8_usize, -60..12_i16, any::<bool>(), proptest::option::of(0..4_usize))
            .prop_map(|(pick, gain, mute, route_bus)| Action::SetStrip { pick, gain, mute, route_bus }),
        1 => (0..8_usize, any::<u8>()).prop_map(|(pick, name)| Action::SetInfo { pick, name }),
        2 => (0..8_usize, 0..400_u16).prop_map(|(track, start)| Action::AddClip { track, start }),
        1 => (0..8_usize, 0..400_u16).prop_map(|(pick, start)| Action::ReplaceClip { pick, start }),
        1 => (0..8_usize).prop_map(Action::RemoveClip),
        1 => Just(Action::AddPattern),
        1 => (0..400_u16).prop_map(Action::AddMarker),
        1 => (0..8_usize).prop_map(Action::RemoveMarker),
        1 => (0..8_usize).prop_map(Action::AddLane),
        1 => any::<u8>().prop_map(Action::SetName),
        1 => (40..300_u16).prop_map(Action::SetTempo),
        3 => Just(Action::Undo),
        2 => Just(Action::Redo),
        1 => Just(Action::Begin),
        1 => Just(Action::End),
        1 => Just(Action::Cancel),
        1 => (1..4_u8).prop_map(Action::Alloc),
        1 => Just(Action::Bogus),
        3 => (any::<bool>(), 0..6_usize)
            .prop_map(|(audio, at)| Action::AddTrackBundle { audio, at }),
        2 => (0..8_usize, 0..3_usize).prop_map(|(pick, index)| Action::AddDevice { pick, index }),
        1 => (0..6_usize).prop_map(Action::RemoveDevice),
        2 => (0..6_usize, 0..4_u8, proptest::option::of(-50..50_i8))
            .prop_map(|(pick, param, value)| Action::SetDeviceParam { pick, param, value }),
        1 => (0..6_usize, any::<u8>()).prop_map(|(pick, n)| Action::SetDeviceState { pick, n }),
        1 => (0..6_usize, any::<bool>())
            .prop_map(|(pick, enabled)| Action::SetDeviceEnabled { pick, enabled }),
        1 => (0..6_usize, proptest::option::of(0..8_usize))
            .prop_map(|(pick, source)| Action::SetDeviceSidechain { pick, source }),
        2 => Just(Action::AddMedia),
        1 => (0..4_usize).prop_map(Action::RemoveMedia),
        1 => (0..4_usize).prop_map(Action::ReplaceMedia),
        1 => (0..8_usize, 0..4_usize).prop_map(|(track, media)| Action::AddAudioClip { track, media }),
        1 => (proptest::option::of((0..50_u8, 51..100_u8)), 0..17_u8)
            .prop_map(|(loop_range, count_in)| Action::SetSettings { loop_range, count_in }),
        1 => (1..13_u8, 0..4_u8)
            .prop_map(|(numerator, shift)| Action::SetTimeSignature { numerator, shift }),
        2 => (0..8_usize, 0..4_usize, -40..6_i8)
            .prop_map(|(pick, bus, level)| Action::SetSends { pick, bus, level }),
        1 => (0..4_usize, 0..4_usize).prop_map(|(a, b)| Action::MoveBus(a, b)),
        1 => (0..4_usize, 1..32_u8).prop_map(|(pick, length)| Action::ReplacePattern { pick, length }),
        2 => (0..8_usize, 0..4_usize).prop_map(|(track, pattern)| Action::AddPatternClip { track, pattern }),
        2 => (0..6_usize).prop_map(Action::MakeUnique),
        1 => (0..6_usize, -50..50_i8).prop_map(|(pick, value)| Action::ReplaceLane { pick, value }),
        1 => (0..6_usize, 0..400_u16).prop_map(|(pick, beat)| Action::ReplaceMarker { pick, beat }),
        1 => Just(Action::Batch),
        1 => Just(Action::FailingBatch),
    ]
}

/// The commands `action` expands to against `session`'s project; `ids` supplies reserved ids.
fn build(
    session: &Session,
    action: &Action,
    ids: &mut impl Iterator<Item = u64>,
) -> Vec<UiCommand> {
    let project = session.project();
    let edit = |edit| vec![UiCommand::Edit { edit }];
    let pick_strip = |pick: usize| -> MixerRef {
        let tracks = project.tracks.len();
        let i = pick % (tracks + project.buses.len() + 1);
        if i < tracks {
            MixerRef::Track(project.tracks[i].id)
        } else if i < tracks + project.buses.len() {
            MixerRef::Bus(project.buses[i - tracks].id)
        } else {
            MixerRef::Master
        }
    };
    match action {
        Action::AddTrack { audio, at } => {
            let Some(id) = ids.next() else {
                return vec![];
            };
            edit(EditCommand::InsertTrack {
                index: (*at).min(project.tracks.len()),
                bundle: Box::new(Owned::new(Track {
                    id: id.into(),
                    name: format!("Track {id}"),
                    color: (id as u32 * 977) & 0xFF_FFFF,
                    kind: if *audio {
                        TrackKind::Audio
                    } else {
                        TrackKind::Midi
                    },
                    strip: Strip::default(),
                    instrument: None,
                })),
            })
        }
        Action::RemoveTrack(i) => match project.tracks.get(*i) {
            Some(t) => edit(EditCommand::RemoveTrack { id: t.id }),
            None => vec![],
        },
        Action::MoveTrack(i, to) => match project.tracks.get(*i) {
            Some(t) => edit(EditCommand::MoveTrack { id: t.id, to: *to }),
            None => vec![],
        },
        Action::AddBus => {
            let Some(id) = ids.next() else {
                return vec![];
            };
            edit(EditCommand::InsertBus {
                index: project.buses.len(),
                bundle: Box::new(Owned::new(Bus {
                    id: id.into(),
                    name: format!("Bus {id}"),
                    color: 0x33_66CC,
                    strip: Strip::default(),
                })),
            })
        }
        Action::RemoveBus(i) => match project.buses.get(*i) {
            Some(b) => edit(EditCommand::RemoveBus { id: b.id }),
            None => vec![],
        },
        Action::SetStrip {
            pick,
            gain,
            mute,
            route_bus,
        } => {
            let target = pick_strip(*pick);
            let output = match (target, route_bus.and_then(|i| project.buses.get(i))) {
                (MixerRef::Master, _) | (_, None) => Output::Master,
                (_, Some(bus)) => Output::Bus(bus.id),
            };
            edit(EditCommand::SetStripSettings {
                target,
                settings: StripSettings {
                    gain_db: f64::from(*gain) / 2.0,
                    mute: *mute,
                    output,
                    ..StripSettings::default()
                },
            })
        }
        Action::SetInfo { pick, name } => edit(EditCommand::SetInfo {
            target: pick_strip(*pick),
            name: format!("n{name}"),
            color: u32::from(*name) * 1000,
        }),
        Action::AddClip { track, start } => {
            let Some(t) = project.tracks.get(*track) else {
                return vec![];
            };
            if t.kind != TrackKind::Midi {
                return vec![];
            }
            let Some(id) = ids.next() else {
                return vec![];
            };
            edit(EditCommand::AddClip {
                clip: Box::new(Clip {
                    id: id.into(),
                    track: t.id,
                    name: format!("Clip {id}"),
                    start: f64::from(*start) / 4.0,
                    length: 4.0,
                    muted: false,
                    content: ClipContent::Midi {
                        notes: vec![Note {
                            start: 0.0,
                            length: 1.0,
                            key: 60,
                            velocity: 100,
                        }],
                        controllers: vec![],
                    },
                }),
            })
        }
        Action::ReplaceClip { pick, start } => {
            let clips: Vec<&Clip> = project.clips.values().collect();
            let Some(clip) = clips.get(*pick % clips.len().max(1)) else {
                return vec![];
            };
            let mut next = (*clip).clone();
            next.start = f64::from(*start) / 4.0;
            next.muted = !next.muted;
            edit(EditCommand::ReplaceClip {
                clip: Box::new(next),
            })
        }
        Action::RemoveClip(i) => {
            let clips: Vec<&Clip> = project.clips.values().collect();
            match clips.get(*i % clips.len().max(1)) {
                Some(c) => edit(EditCommand::RemoveClip { id: c.id }),
                None => vec![],
            }
        }
        Action::AddPattern => {
            let Some(id) = ids.next() else {
                return vec![];
            };
            edit(EditCommand::AddPattern {
                pattern: Pattern {
                    id: id.into(),
                    name: format!("Pattern {id}"),
                    color: 0x00_FF00,
                    length: 4.0,
                    notes: vec![Note {
                        start: 0.5,
                        length: 0.5,
                        key: 64,
                        velocity: 90,
                    }],
                },
            })
        }
        Action::AddMarker(beat) => {
            let Some(id) = ids.next() else {
                return vec![];
            };
            edit(EditCommand::AddMarker {
                marker: Marker {
                    id: id.into(),
                    name: format!("M{id}"),
                    beat: f64::from(*beat),
                    color: 0xFF_0000,
                },
            })
        }
        Action::RemoveMarker(i) => {
            let markers: Vec<_> = project.markers.keys().copied().collect();
            match markers.get(*i % markers.len().max(1)) {
                Some(id) => edit(EditCommand::RemoveMarker { id: *id }),
                None => vec![],
            }
        }
        Action::AddLane(pick) => {
            let Some(id) = ids.next() else {
                return vec![];
            };
            edit(EditCommand::AddLane {
                lane: AutomationLane {
                    id: id.into(),
                    target: AutomationTarget::Strip {
                        strip: pick_strip(*pick),
                        param: StripParam::Pan,
                    },
                    points: vec![AutomationPoint {
                        beat: 0.0,
                        value: 0.0,
                        curve: 0.0,
                    }],
                    enabled: true,
                },
            })
        }
        Action::SetName(n) => edit(EditCommand::SetName {
            name: format!("Project {n}"),
        }),
        Action::SetTempo(bpm) => edit(EditCommand::SetTempoMap {
            tempo_map: ow_model::time::TempoMap::constant(f64::from(*bpm)).unwrap(),
        }),
        Action::Undo => vec![UiCommand::Undo],
        Action::Redo => vec![UiCommand::Redo],
        Action::Begin => vec![UiCommand::Begin {
            label: "Gesture".into(),
        }],
        Action::End => vec![UiCommand::End],
        Action::Cancel => vec![UiCommand::Cancel],
        Action::Alloc(n) => vec![UiCommand::AllocIds {
            count: u32::from(*n),
        }],
        Action::Bogus => edit(EditCommand::RemoveTrack {
            id: 999_999_999.into(),
        }),
        Action::AddTrackBundle { audio, at } => {
            let (Some(track), Some(device), Some(lane_a), Some(lane_b), Some(clip)) =
                (ids.next(), ids.next(), ids.next(), ids.next(), ids.next())
            else {
                return vec![];
            };
            let owned = Owned {
                item: Track {
                    id: track.into(),
                    name: format!("Owner {track}"),
                    color: 0x12_3456,
                    kind: if *audio {
                        TrackKind::Audio
                    } else {
                        TrackKind::Midi
                    },
                    strip: Strip {
                        settings: StripSettings::default(),
                        sends: vec![],
                        inserts: vec![device.into()],
                    },
                    instrument: None,
                },
                clips: if *audio {
                    vec![]
                } else {
                    vec![Clip {
                        id: clip.into(),
                        track: track.into(),
                        name: "Owned clip".into(),
                        start: 1.0,
                        length: 2.0,
                        muted: false,
                        content: ClipContent::Midi {
                            notes: vec![],
                            controllers: vec![],
                        },
                    }]
                },
                devices: vec![device_of(device, "Owned device")],
                lanes: vec![
                    AutomationLane {
                        id: lane_a.into(),
                        target: AutomationTarget::Device {
                            device: device.into(),
                            param: 0,
                        },
                        points: vec![],
                        enabled: true,
                    },
                    AutomationLane {
                        id: lane_b.into(),
                        target: AutomationTarget::Strip {
                            strip: MixerRef::Track(track.into()),
                            param: StripParam::Gain,
                        },
                        points: vec![],
                        enabled: false,
                    },
                ],
            };
            edit(EditCommand::InsertTrack {
                index: (*at).min(project.tracks.len()),
                bundle: Box::new(owned),
            })
        }
        Action::AddDevice { pick, index } => {
            let (Some(device), Some(lane)) = (ids.next(), ids.next()) else {
                return vec![];
            };
            let strip = pick_strip(*pick);
            edit(EditCommand::InsertDevice {
                slot: DeviceSlot::Insert {
                    strip,
                    index: *index,
                },
                bundle: Box::new(DeviceBundle {
                    device: device_of(device, "Effect"),
                    lanes: vec![AutomationLane {
                        id: lane.into(),
                        target: AutomationTarget::Device {
                            device: device.into(),
                            param: 1,
                        },
                        points: vec![AutomationPoint {
                            beat: 0.0,
                            value: 0.5,
                            curve: 0.0,
                        }],
                        enabled: true,
                    }],
                }),
            })
        }
        Action::RemoveDevice(i) => match nth_key(&project.devices, *i) {
            Some(id) => edit(EditCommand::RemoveDevice { id }),
            None => vec![],
        },
        Action::SetDeviceParam { pick, param, value } => match nth_key(&project.devices, *pick) {
            Some(device) => edit(EditCommand::SetDeviceParam {
                device,
                param: u32::from(*param),
                value: value.map(|v| f64::from(v) / 10.0),
            }),
            None => vec![],
        },
        Action::SetDeviceState { pick, n } => match nth_key(&project.devices, *pick) {
            Some(device) => edit(EditCommand::SetDeviceState {
                device,
                state: vec![StateChunk {
                    format: "builtin".into(),
                    data: format!("state-{n}"),
                }],
            }),
            None => vec![],
        },
        Action::SetDeviceEnabled { pick, enabled } => match nth_key(&project.devices, *pick) {
            Some(device) => edit(EditCommand::SetDeviceEnabled {
                device,
                enabled: *enabled,
            }),
            None => vec![],
        },
        Action::SetDeviceSidechain { pick, source } => match nth_key(&project.devices, *pick) {
            Some(device) => edit(EditCommand::SetDeviceSidechain {
                device,
                source: source.map(pick_strip),
            }),
            None => vec![],
        },
        Action::AddMedia => {
            let Some(id) = ids.next() else {
                return vec![];
            };
            edit(EditCommand::AddMedia {
                item: media_of(id, 48_000),
            })
        }
        Action::RemoveMedia(i) => match nth_key(&project.media_pool, *i) {
            Some(id) => edit(EditCommand::RemoveMedia { id }),
            None => vec![],
        },
        Action::ReplaceMedia(i) => match nth_key(&project.media_pool, *i) {
            Some(id) => edit(EditCommand::ReplaceMedia {
                item: media_of(id.0, 44_100),
            }),
            None => vec![],
        },
        Action::AddAudioClip { track, media } => {
            let (Some(t), Some(media)) = (
                project.tracks.get(*track),
                nth_key(&project.media_pool, *media),
            ) else {
                return vec![];
            };
            if t.kind != TrackKind::Audio {
                return vec![];
            }
            let Some(id) = ids.next() else {
                return vec![];
            };
            edit(EditCommand::AddClip {
                clip: Box::new(Clip {
                    id: id.into(),
                    track: t.id,
                    name: format!("Audio {id}"),
                    start: 2.0,
                    length: 3.0,
                    muted: false,
                    content: ClipContent::Audio {
                        media,
                        source_offset: 10,
                        gain_db: -3.0,
                        fade_in: 0.25,
                        fade_out: 0.5,
                    },
                }),
            })
        }
        Action::SetSettings {
            loop_range,
            count_in,
        } => {
            let mut settings = project.settings.clone();
            settings.loop_range = loop_range.map(|(a, b)| BeatRange {
                start: f64::from(a),
                end: f64::from(b),
            });
            settings.count_in_bars = *count_in;
            edit(EditCommand::SetSettings { settings })
        }
        Action::SetTimeSignature { numerator, shift } => edit(EditCommand::SetTimeSignatureMap {
            map: TimeSignatureMap::from_signature(
                TimeSignature::new(u32::from(*numerator), 1 << (shift + 1)).unwrap(),
            ),
        }),
        Action::SetSends { pick, bus, level } => {
            let target = pick_strip(*pick);
            let Some(destination) = project.buses.get(*bus) else {
                return vec![];
            };
            if matches!(target, MixerRef::Master) {
                return vec![];
            }
            edit(EditCommand::SetSends {
                target,
                sends: vec![AuxSend {
                    target: destination.id,
                    level_db: f64::from(*level),
                    pre_fader: level % 2 == 0,
                }],
            })
        }
        Action::MoveBus(i, to) => match project.buses.get(*i) {
            Some(b) => edit(EditCommand::MoveBus { id: b.id, to: *to }),
            None => vec![],
        },
        Action::ReplacePattern { pick, length } => match nth_key(&project.patterns, *pick) {
            Some(id) => {
                let mut pattern = project.patterns[&id].clone();
                pattern.length = f64::from(*length);
                pattern.notes.clear();
                edit(EditCommand::ReplacePattern { pattern })
            }
            None => vec![],
        },
        Action::AddPatternClip { track, pattern } => {
            let (Some(t), Some(pattern)) = (
                project.tracks.get(*track),
                nth_key(&project.patterns, *pattern),
            ) else {
                return vec![];
            };
            if t.kind != TrackKind::Midi {
                return vec![];
            }
            let Some(id) = ids.next() else {
                return vec![];
            };
            edit(EditCommand::AddClip {
                clip: Box::new(Clip {
                    id: id.into(),
                    track: t.id,
                    name: format!("Instance {id}"),
                    start: 8.0,
                    length: 4.0,
                    muted: false,
                    content: ClipContent::Pattern { pattern },
                }),
            })
        }
        Action::MakeUnique(pick) => {
            let instances: Vec<_> = project
                .clips
                .values()
                .filter(|c| matches!(c.content, ClipContent::Pattern { .. }))
                .collect();
            let Some(clip) = instances.get(*pick % instances.len().max(1)) else {
                return vec![];
            };
            let Some(id) = ids.next() else {
                return vec![];
            };
            edit(EditCommand::MakeUnique {
                clip: clip.id,
                pattern: id.into(),
            })
        }
        Action::ReplaceLane { pick, value } => match nth_key(&project.automation, *pick) {
            Some(id) => {
                let mut lane = project.automation[&id].clone();
                lane.points = vec![AutomationPoint {
                    beat: 1.0,
                    value: f64::from(*value),
                    curve: 0.5,
                }];
                lane.enabled = !lane.enabled;
                edit(EditCommand::ReplaceLane { lane })
            }
            None => vec![],
        },
        Action::ReplaceMarker { pick, beat } => match nth_key(&project.markers, *pick) {
            Some(id) => {
                let mut marker = project.markers[&id].clone();
                marker.beat = f64::from(*beat);
                marker.name.push('!');
                edit(EditCommand::ReplaceMarker { marker })
            }
            None => vec![],
        },
        Action::Batch => {
            let Some(id) = ids.next() else {
                return vec![];
            };
            edit(EditCommand::Batch {
                commands: vec![
                    EditCommand::SetName {
                        name: format!("Batch {id}"),
                    },
                    EditCommand::AddMarker {
                        marker: Marker {
                            id: id.into(),
                            name: "batched".into(),
                            beat: 3.0,
                            color: 0,
                        },
                    },
                ],
            })
        }
        Action::FailingBatch => edit(EditCommand::Batch {
            commands: vec![
                EditCommand::SetName {
                    name: "never applied".into(),
                },
                EditCommand::RemoveTrack {
                    id: 999_999_999.into(),
                },
            ],
        }),
    }
}

fn nth_key<K: Copy, V>(map: &std::collections::BTreeMap<K, V>, pick: usize) -> Option<K> {
    let keys: Vec<K> = map.keys().copied().collect();
    keys.get(pick % keys.len().max(1)).copied()
}

fn device_of(id: u64, name: &str) -> Device {
    Device {
        id: id.into(),
        kind: "builtin.gain".into(),
        name: name.into(),
        enabled: true,
        params: std::collections::BTreeMap::new(),
        state: vec![],
        sidechain: None,
    }
}

fn media_of(id: u64, rate: u32) -> MediaItem {
    MediaItem {
        id: id.into(),
        name: format!("Media {id}"),
        path: format!("audio/m{id}.wav"),
        hash: format!("{id:08x}"),
        sample_rate: rate,
        channels: 2,
        frames: 1000 + id,
    }
}

/// Runs `action` against `session`, handing every patch it produces (with the session as it is
/// right after that patch) to `deliver` in order. Six fresh ids are reserved first, like the UI
/// does (the reservation is a patch like any other).
pub fn run(session: &mut Session, action: &Action, mut deliver: impl FnMut(&Session, ModelPatch)) {
    let reserve = session.handle(UiCommand::AllocIds { count: 6 });
    let mut ids = reserve.ids.clone().into_iter();
    if let Some(patch) = reserve.patch {
        deliver(session, patch);
    }
    let marker = ids.as_slice().first().copied();
    for command in build(session, action, &mut ids) {
        if let Some(patch) = session.handle(command).patch {
            deliver(session, patch);
        }
    }
    // Now and then the project is saved somewhere (path and dirty flag change, the model does
    // not) or the engine reports media it cannot load.
    if let Some(id) = marker {
        let patch = match id % 7 {
            0 => session.set_project_path(Some(format!("/projects/p{id}.owave"))),
            3 => session.mark_saved(),
            5 => session.set_missing_media(vec![id, id + 1]),
            1 => session.set_armed_tracks(vec![id, id + 2]),
            _ => None,
        };
        if let Some(patch) = patch {
            deliver(session, patch);
        }
    }
}
