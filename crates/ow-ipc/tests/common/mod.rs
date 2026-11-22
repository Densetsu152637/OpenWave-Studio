//! Shared by the mirror tests and the fixture generator: abstract actions that are turned into
//! concrete UI commands against the session project, so random sequences hit real entities (and
//! sometimes deliberately invalid ones).

#![allow(dead_code)]

use ow_ipc::{EditCommand, MixerRef, ModelPatch, Session, UiCommand};
use ow_model::{
    AutomationLane, AutomationPoint, AutomationTarget, Bus, Clip, ClipContent, Marker, Note,
    Output, Owned, Pattern, Strip, StripParam, StripSettings, Track, TrackKind,
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
    }
}

/// Runs `action` against `session`, handing every patch it produces (with the session as it is
/// right after that patch) to `deliver` in order. One fresh id is reserved first, like the UI
/// does (the reservation is a patch like any other).
pub fn run(session: &mut Session, action: &Action, mut deliver: impl FnMut(&Session, ModelPatch)) {
    let reserve = session.handle(UiCommand::AllocIds { count: 1 });
    let mut ids = reserve.ids.clone().into_iter();
    if let Some(patch) = reserve.patch {
        deliver(session, patch);
    }
    for command in build(session, action, &mut ids) {
        if let Some(patch) = session.handle(command).patch {
            deliver(session, patch);
        }
    }
}
