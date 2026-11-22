//! The compiler from the project model to an engine [`RenderPlan`] and [`Timeline`].
//!
//! The control thread owns the authoritative [`Project`]; after an edit it calls
//! [`compile_project`], then sends [`Compiled::plan`] with
//! [`ControlLink::swap_plan`](crate::ControlLink::swap_plan) and [`Compiled::timeline`] with
//! [`ControlLink::swap_timeline`](crate::ControlLink::swap_timeline).
//!
//! # What is compiled so far
//!
//! Every strip becomes a chain `[source] -> inserts -> fader`, and faders feed their output
//! target:
//!
//! * a track's fader is a [`GainNode`] carrying the strip gain, polarity, mute and solo (solo
//!   follows the track rule: when any track is soloed, other tracks are silent unless
//!   solo-safe; bus solo is not interpreted yet);
//! * a bus's fader is a [`GainNode`] that sums what routes into it; the master's fader is the
//!   stereo [`GainNode`] whose outputs are the plan output;
//! * sends are [`GainNode`]s tapped pre- or post-fader that feed the target bus;
//! * pan is not applied yet (the mixer stage owns the pan law);
//! * clips and devices are placeholders until their stages exist: a track with clips gets a
//!   silent [`TestToneNode`] as source (key [`NodeKey::Clips`]; set its
//!   [`TONE_PARAM_ON`](crate::graph::nodes::TONE_PARAM_ON) parameter to hear it), an instrument
//!   device is the same kind of source (key [`NodeKey::Device`]), and an insert device is a
//!   unity [`GainNode`] pass-through.
//!
//! # Stable identity and state carry-over
//!
//! Engine [`NodeId`]s come from insertion order, so they are not model ids. The compiler gives
//! every node a [`NodeKey`] derived from model ids and returns the key to id table
//! ([`NodeMap`]). Nodes are created master first, then strips in model-id order, then sends, so
//! node ids are only usually stable: appending a strip after sends exist shifts the ids of the
//! sends. Consumers must never compare raw ids across compiles; [`NodeMap::diff`] says which
//! keys survive with which old and new ids. Compiled nodes hold no
//! state worth keeping yet; when a later stage adds stateful nodes (device DSP, voices, delay
//! lines) it moves the old node's state into the node with the same key before the new plan is
//! swapped in, using this table. Parameters that change without a rebuild (mute, solo, fader)
//! can be pushed with [`fader_gain`] and [`Message::SetParam`](crate::Message::SetParam).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use ow_model::{
    AuxSend, BusId, DeviceId, MixerRef, Output, Project, Strip, TrackId, ValidationError,
};

use ow_ipc::TelemetryBank;

use crate::graph::nodes::{GainNode, TestToneNode};
use crate::{Graph, GraphError, NodeId, RenderPlan, Timeline};

/// Identity of a compiled node in model terms: stable across recompiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeKey {
    /// Placeholder source for a track's clips.
    Clips(TrackId),
    /// A device (an instrument source or an insert effect).
    Device(DeviceId),
    /// A strip's fader; the master's is the output gain.
    Fader(MixerRef),
    /// A send: the `nth` send from `from` to bus `to`.
    Send {
        /// The sending strip.
        from: MixerRef,
        /// The destination bus.
        to: BusId,
        /// Ordinal among that strip's sends to the same bus.
        nth: u32,
    },
}

/// The table from [`NodeKey`] to the [`NodeId`] a plan assigned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeMap {
    ids: BTreeMap<NodeKey, NodeId>,
}

/// One key present in both compiles: where its state has to move from and to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Carry {
    /// The key.
    pub key: NodeKey,
    /// Node id in the plan being replaced.
    pub old: NodeId,
    /// Node id in the new plan.
    pub new: NodeId,
}

/// What changed between two compiles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeDiff {
    /// Keys in both plans (state carries over).
    pub carried: Vec<Carry>,
    /// Keys only in the new plan (fresh nodes).
    pub added: Vec<(NodeKey, NodeId)>,
    /// Keys only in the old plan (their state is dropped).
    pub removed: Vec<(NodeKey, NodeId)>,
}

impl NodeMap {
    /// The node compiled for `key`.
    pub fn get(&self, key: NodeKey) -> Option<NodeId> {
        self.ids.get(&key).copied()
    }

    /// The fader node of a strip.
    pub fn fader(&self, strip: MixerRef) -> Option<NodeId> {
        self.get(NodeKey::Fader(strip))
    }

    /// Number of mapped nodes (the compiler adds sum and delay nodes on top of these).
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// All entries in key order.
    pub fn iter(&self) -> impl Iterator<Item = (NodeKey, NodeId)> + '_ {
        self.ids.iter().map(|(k, v)| (*k, *v))
    }

    /// Compares the plan this map belongs to (`self`, the live one) with the next.
    pub fn diff(&self, next: &NodeMap) -> NodeDiff {
        let mut diff = NodeDiff::default();
        for (key, new) in &next.ids {
            match self.ids.get(key) {
                Some(old) => diff.carried.push(Carry {
                    key: *key,
                    old: *old,
                    new: *new,
                }),
                None => diff.added.push((*key, *new)),
            }
        }
        for (key, old) in &self.ids {
            if !next.ids.contains_key(key) {
                diff.removed.push((*key, *old));
            }
        }
        diff
    }
}

/// The result of compiling a project.
pub struct Compiled {
    /// The render plan, ready for [`ControlLink::swap_plan`](crate::ControlLink::swap_plan).
    pub plan: RenderPlan,
    /// Tempo and time-signature maps for
    /// [`ControlLink::swap_timeline`](crate::ControlLink::swap_timeline).
    pub timeline: Timeline,
    /// Model key to node id table.
    pub nodes: NodeMap,
}

/// Why a project could not be compiled.
#[derive(Debug, Clone, PartialEq)]
pub enum CompileError {
    /// The project breaks a model invariant (the compiler relies on them).
    Model(ValidationError),
    /// The graph rejected what was built (an engine bug or an unsupported shape).
    Graph(GraphError),
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(e) => write!(f, "invalid project: {e}"),
            Self::Graph(e) => write!(f, "graph error: {e}"),
        }
    }
}

impl std::error::Error for CompileError {}

impl From<GraphError> for CompileError {
    fn from(error: GraphError) -> Self {
        Self::Graph(error)
    }
}

/// Linear gain of a strip's fader as compiled: the fader level and polarity, zero when muted
/// or silenced by another track's solo. Use it to update a fader with a
/// [`Message::SetParam`](crate::Message::SetParam) (parameter
/// [`GAIN_PARAM`](crate::graph::nodes::GAIN_PARAM)) without recompiling.
pub fn fader_gain(project: &Project, strip: MixerRef) -> f32 {
    let Some(s) = project.strip(strip) else {
        return 0.0;
    };
    if !strip_audible(project, strip) {
        return 0.0;
    }
    let linear = db_to_linear(s.settings.gain_db);
    if s.settings.invert_phase {
        -linear
    } else {
        linear
    }
}

/// False when the strip is muted or silenced by another track's solo.
pub fn strip_audible(project: &Project, strip: MixerRef) -> bool {
    let Some(s) = project.strip(strip) else {
        return false;
    };
    let settings = &s.settings;
    !settings.mute
        && match strip {
            MixerRef::Track(_) => {
                let any_solo = project.tracks.iter().any(|t| t.strip.settings.solo);
                !any_solo || settings.solo || settings.solo_safe
            }
            MixerRef::Bus(_) | MixerRef::Master => true,
        }
}

/// Linear gain of one of a strip's sends as compiled: the send level, zero when the strip is
/// not audible (pre-fader sends respect mute and solo too).
pub fn send_gain(project: &Project, strip: MixerRef, send: &AuxSend) -> f32 {
    if strip_audible(project, strip) {
        db_to_linear(send.level_db)
    } else {
        0.0
    }
}

fn db_to_linear(db: f64) -> f32 {
    10.0_f64.powf(db / 20.0) as f32
}

/// Where a signal can be tapped: a node and whether it has one output channel (mono sources) or
/// two.
#[derive(Clone, Copy)]
struct Tap {
    node: NodeId,
    mono: bool,
}

/// What `build_strip` leaves for wiring.
struct StripNodes {
    /// First node of the strip's chain, which incoming signals connect to (stereo input).
    entry: NodeId,
    /// The signal before the fader (pre-fader sends tap here); `None` when the strip has no
    /// source and no inserts, in which case pre-fader sends tap the fader instead (a bus
    /// without inserts has no node before its fader until the mixer stage adds an input stage).
    pre: Option<Tap>,
    /// The fader.
    fader: NodeId,
}

fn connect_stereo(graph: &mut Graph, from: Tap, to: NodeId) -> Result<(), GraphError> {
    graph.connect(from.node, 0, to, 0)?;
    graph.connect(from.node, if from.mono { 0 } else { 1 }, to, 1)
}

/// Compiles `project` for `sample_rate` and blocks of up to `max_block` frames.
///
/// Allocates freely: run it on the control thread.
pub fn compile_project(
    project: &Project,
    sample_rate: u32,
    max_block: usize,
) -> Result<Compiled, CompileError> {
    compile_project_metered(project, sample_rate, max_block, None)
}

/// Like [`compile_project`], and when `meters` is given every strip's fader publishes its peak
/// and RMS to the bank slot [`ow_ipc::meter_slot`] assigns it (the UI reads them through the
/// telemetry channel). Strips beyond the bank's slots are not metered.
pub fn compile_project_metered(
    project: &Project,
    sample_rate: u32,
    max_block: usize,
    meters: Option<&Arc<TelemetryBank>>,
) -> Result<Compiled, CompileError> {
    project.validate().map_err(CompileError::Model)?;

    let mut graph = Graph::new();
    let mut nodes = NodeMap::default();
    let mut add = |graph: &mut Graph, key: NodeKey, node: Box<dyn crate::Node>| {
        let id = graph.add_node(node);
        nodes.ids.insert(key, id);
        id
    };

    // Master first, then every other strip in model-id order, so appending entities keeps every
    // existing node id.
    let master = build_strip(
        &mut graph,
        &mut add,
        project,
        MixerRef::Master,
        &project.master,
        meters,
    );
    let mut strips: Vec<(u64, MixerRef)> = project
        .tracks
        .iter()
        .map(|t| (t.id.0, MixerRef::Track(t.id)))
        .chain(project.buses.iter().map(|b| (b.id.0, MixerRef::Bus(b.id))))
        .collect();
    strips.sort_unstable();
    let mut built: BTreeMap<MixerRef, StripNodes> = BTreeMap::new();
    for (_, target) in &strips {
        if let Some(strip) = project.strip(*target) {
            let nodes = build_strip(&mut graph, &mut add, project, *target, strip, meters);
            built.insert(*target, nodes);
        }
    }
    built.insert(MixerRef::Master, master);
    let entry_of = |output: Output, built: &BTreeMap<MixerRef, StripNodes>| match output {
        Output::Master => built[&MixerRef::Master].entry,
        Output::Bus(bus) => built[&MixerRef::Bus(bus)].entry,
    };

    // Wiring: fader outputs to their targets; sends in (strip id, target, ordinal) order.
    for (_, target) in &strips {
        let strip = project.strip(*target).expect("strip exists");
        let nodes_of = &built[target];
        let dest = entry_of(strip.settings.output, &built);
        graph.connect(nodes_of.fader, 0, dest, 0)?;
        graph.connect(nodes_of.fader, 1, dest, 1)?;
    }
    let master_fader = built[&MixerRef::Master].fader;
    graph.set_master(master_fader)?;

    for (_, target) in &strips {
        let strip = project.strip(*target).expect("strip exists");
        let nodes_of = &built[target];
        let mut counts: BTreeMap<BusId, u32> = BTreeMap::new();
        for send in &strip.sends {
            let nth = counts.entry(send.target).or_insert(0);
            let key = NodeKey::Send {
                from: *target,
                to: send.target,
                nth: *nth,
            };
            *nth += 1;
            let node = add(
                &mut graph,
                key,
                Box::new(GainNode::new(2, send_gain(project, *target, send))),
            );
            let post = Tap {
                node: nodes_of.fader,
                mono: false,
            };
            let tap = if send.pre_fader {
                nodes_of.pre.unwrap_or(post)
            } else {
                post
            };
            connect_stereo(&mut graph, tap, node)?;
            let dest = built[&MixerRef::Bus(send.target)].entry;
            graph.connect(node, 0, dest, 0)?;
            graph.connect(node, 1, dest, 1)?;
        }
    }

    let plan = graph.compile(sample_rate, max_block)?;
    let timeline = Timeline::new(
        project.tempo_map.clone(),
        project.time_signature_map.clone(),
    );
    Ok(Compiled {
        plan,
        timeline,
        nodes,
    })
}

type AddNode<'a> = dyn FnMut(&mut Graph, NodeKey, Box<dyn crate::Node>) -> NodeId + 'a;

/// Adds the nodes of one strip and wires source, inserts and fader together.
fn build_strip(
    graph: &mut Graph,
    add: &mut AddNode<'_>,
    project: &Project,
    target: MixerRef,
    strip: &Strip,
    meters: Option<&Arc<TelemetryBank>>,
) -> StripNodes {
    // Source: a placeholder for the track's clips or for its instrument.
    let mut source: Option<Tap> = None;
    if let MixerRef::Track(id) = target
        && let Some(track) = project.track(id)
    {
        if let Some(instrument) = track.instrument {
            let node = add(
                graph,
                NodeKey::Device(instrument),
                Box::<TestToneNode>::default(),
            );
            source = Some(Tap { node, mono: true });
        } else if project.clips.values().any(|c| c.track == id) {
            let node = add(graph, NodeKey::Clips(id), Box::<TestToneNode>::default());
            source = Some(Tap { node, mono: true });
        }
    }

    // Inserts: unity pass-through placeholders in chain order.
    let mut previous = source;
    let mut entry: Option<NodeId> = None;
    let mut wire: Vec<(Tap, NodeId)> = Vec::new();
    for device in &strip.inserts {
        let node = add(
            graph,
            NodeKey::Device(*device),
            Box::new(GainNode::new(2, 1.0)),
        );
        entry.get_or_insert(node);
        if let Some(from) = previous {
            wire.push((from, node));
        }
        previous = Some(Tap { node, mono: false });
    }

    // The master's fader is a plain stereo gain too: its two outputs are the plan's output.
    let mut fader_node = GainNode::new(2, fader_gain(project, target));
    if let Some(bank) = meters
        && let Some(slot) = ow_ipc::meter_slot(project, target)
    {
        fader_node = fader_node.with_meter(Arc::clone(bank), slot);
    }
    let fader = add(graph, NodeKey::Fader(target), Box::new(fader_node));
    let entry = entry.unwrap_or(fader);
    if let Some(from) = previous {
        wire.push((from, fader));
    }
    // Every id here was just added with matching channel counts, so these cannot fail.
    for (from, to) in wire {
        let connected = connect_stereo(graph, from, to);
        debug_assert!(connected.is_ok());
    }
    StripNodes {
        entry,
        pre: previous,
        fader,
    }
}

#[cfg(test)]
mod tests {
    use ow_ipc::TelemetryBank;
    use ow_model::{
        AuxSend, Bus, Clip, ClipContent, Device, EditCommand, Editor, MediaItem, Owned, Strip,
        StripSettings, Track, TrackKind,
    };

    use super::*;
    use crate::graph::nodes::{GAIN_PARAM, TONE_PARAM_ON};
    use crate::{Message, OfflineRenderer};

    const RATE: u32 = 48_000;
    const FRAMES: usize = 24_000;

    fn media(ed: &mut Editor) -> ow_model::MediaId {
        let id = ed.alloc_id().unwrap();
        ed.apply(EditCommand::AddMedia {
            item: MediaItem {
                id,
                name: "m".into(),
                path: "audio/m.wav".into(),
                hash: "00".into(),
                sample_rate: RATE,
                channels: 1,
                frames: 1,
            },
        })
        .unwrap();
        id
    }

    /// An audio track with one clip (so it compiles a source) and the given strip settings.
    fn audio_track(ed: &mut Editor, media: ow_model::MediaId, settings: StripSettings) -> TrackId {
        let id: TrackId = ed.alloc_id().unwrap();
        let index = ed.project().tracks.len();
        ed.apply(EditCommand::InsertTrack {
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
            })),
        })
        .unwrap();
        let clip = ed.alloc_id().unwrap();
        ed.apply(EditCommand::AddClip {
            clip: Box::new(Clip {
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
            }),
        })
        .unwrap();
        id
    }

    fn bus(ed: &mut Editor, settings: StripSettings) -> BusId {
        let id: BusId = ed.alloc_id().unwrap();
        let index = ed.project().buses.len();
        ed.apply(EditCommand::InsertBus {
            index,
            bundle: Box::new(Owned::new(Bus {
                id,
                name: "b".into(),
                color: 0,
                strip: Strip {
                    settings,
                    ..Strip::default()
                },
            })),
        })
        .unwrap();
        id
    }

    fn db(gain_db: f64) -> StripSettings {
        StripSettings {
            gain_db,
            ..StripSettings::default()
        }
    }

    /// Renders with the given source tones switched on; returns the RMS of the left channel
    /// over the last 4096 frames.
    fn level(project: &Project, tones: &[NodeKey]) -> f32 {
        let compiled = compile_project(project, RATE, 256).unwrap();
        let mut renderer = OfflineRenderer::new(compiled.plan, 2);
        for key in tones {
            let node = compiled.nodes.get(*key).expect("tone node");
            assert!(renderer.send(Message::SetParam {
                node,
                param: TONE_PARAM_ON,
                value: 1.0,
            }));
        }
        let out = renderer.render(FRAMES, 256);
        let tail = &out[(FRAMES - 4096) * 2..];
        let sum: f32 = tail.chunks(2).map(|f| f[0] * f[0]).sum();
        (sum / 4096.0).sqrt()
    }

    /// RMS of the 0.2-amplitude sine source.
    const SOURCE_RMS: f32 = 0.2 * std::f32::consts::FRAC_1_SQRT_2;

    #[test]
    fn compile_empty_project_renders_silence_with_the_models_timeline() {
        let project = Project::new("empty");
        let compiled = compile_project(&project, RATE, 256).unwrap();
        assert_eq!(compiled.timeline.tempo(), &project.tempo_map);
        assert_eq!(compiled.timeline.signatures(), &project.time_signature_map);
        assert_eq!(compiled.nodes.len(), 1, "only the master fader");
        assert_eq!(compiled.plan.master_channels(), 2);
        let mut renderer = OfflineRenderer::new(compiled.plan, 2);
        assert!(renderer.render(2048, 256).iter().all(|s| *s == 0.0));
    }

    #[test]
    fn compile_track_gain_reaches_the_master() {
        let mut ed = Editor::new(Project::new("p"));
        let m = media(&mut ed);
        let a = audio_track(&mut ed, m, db(0.0));
        let b = audio_track(&mut ed, m, db(-6.0206));
        let only_a = level(ed.project(), &[NodeKey::Clips(a)]);
        let only_b = level(ed.project(), &[NodeKey::Clips(b)]);
        assert!((only_a - SOURCE_RMS).abs() < 1e-3, "{only_a}");
        assert!((only_b - SOURCE_RMS / 2.0).abs() < 1e-3, "{only_b}");
        // Without clips a track has no source node at all.
        let empty = compile_project(&Project::new("x"), RATE, 64).unwrap();
        assert!(empty.nodes.get(NodeKey::Clips(a)).is_none());
    }

    #[test]
    fn compile_mute_solo_and_phase() {
        let mut ed = Editor::new(Project::new("p"));
        let m = media(&mut ed);
        let muted = StripSettings {
            mute: true,
            ..db(0.0)
        };
        let safe = StripSettings {
            solo_safe: true,
            ..db(0.0)
        };
        let a = audio_track(&mut ed, m, muted);
        let b = audio_track(&mut ed, m, db(0.0));
        let c = audio_track(&mut ed, m, safe);
        assert_eq!(level(ed.project(), &[NodeKey::Clips(a)]), 0.0);
        assert!(level(ed.project(), &[NodeKey::Clips(b)]) > 0.1);

        // Soloing `b` silences `a` and `c` unless solo-safe.
        let mut soloed = ed.project().clone();
        soloed.tracks[1].strip.settings.solo = true;
        assert!(level(&soloed, &[NodeKey::Clips(b)]) > 0.1);
        assert!(
            level(&soloed, &[NodeKey::Clips(c)]) > 0.1,
            "solo-safe track stays"
        );
        soloed.tracks[2].strip.settings.solo_safe = false;
        assert_eq!(level(&soloed, &[NodeKey::Clips(c)]), 0.0);

        // Polarity inversion cancels an identical track.
        let mut inverted = ed.project().clone();
        inverted.tracks[0].strip.settings.mute = false;
        inverted.tracks[1].strip.settings.invert_phase = true;
        assert!(level(&inverted, &[NodeKey::Clips(a), NodeKey::Clips(b)]) < 1e-3);

        assert_eq!(fader_gain(&soloed, MixerRef::Track(a)), 0.0);
        assert_eq!(fader_gain(&soloed, MixerRef::Track(b)), 1.0);
        assert_eq!(fader_gain(&inverted, MixerRef::Track(b)), -1.0);
    }

    #[test]
    fn compile_bus_routing_and_sends() {
        let mut ed = Editor::new(Project::new("p"));
        let m = media(&mut ed);
        let drums = bus(&mut ed, db(-6.0206));
        let to_bus = StripSettings {
            output: Output::Bus(drums),
            ..db(0.0)
        };
        let track = audio_track(&mut ed, m, to_bus);
        let key = [NodeKey::Clips(track)];
        // Routed through a -6 dB bus.
        let routed = level(ed.project(), &key);
        assert!((routed - SOURCE_RMS / 2.0).abs() < 1e-3, "{routed}");

        // Direct to master plus a post-fader -6 dB send to the bus: 1 + 0.5 x 0.5.
        let mut with_send = ed.project().clone();
        with_send.tracks[0].strip.settings.output = Output::Master;
        with_send.tracks[0].strip.sends = vec![AuxSend {
            target: drums,
            level_db: -6.0206,
            pre_fader: false,
        }];
        let sent = level(&with_send, &key);
        assert!((sent - SOURCE_RMS * 1.25).abs() < 2e-3, "{sent}");

        // A pre-fader send ignores the track fader: fader nearly off, send still audible.
        with_send.tracks[0].strip.settings.gain_db = -100.0;
        with_send.tracks[0].strip.sends[0].pre_fader = true;
        let pre = level(&with_send, &key);
        assert!((pre - SOURCE_RMS * 0.25).abs() < 2e-3, "{pre}");
        let compiled = compile_project(&with_send, RATE, 256).unwrap();
        let send_key = NodeKey::Send {
            from: MixerRef::Track(track),
            to: drums,
            nth: 0,
        };
        assert!(compiled.nodes.get(send_key).is_some());
    }

    #[test]
    fn compile_pre_fader_sends_respect_mute_and_solo() {
        let mut ed = Editor::new(Project::new("p"));
        let m = media(&mut ed);
        let aux = bus(&mut ed, db(0.0));
        let a = audio_track(&mut ed, m, db(0.0));
        let b = audio_track(&mut ed, m, db(0.0));
        // Both tracks send pre-fader to the bus; their own outputs are turned down.
        let mut project = ed.project().clone();
        for track in &mut project.tracks {
            track.strip.settings.gain_db = -100.0;
            track.strip.sends = vec![AuxSend {
                target: aux,
                level_db: 0.0,
                pre_fader: true,
            }];
        }
        let audible = level(&project, &[NodeKey::Clips(a)]);
        assert!((audible - SOURCE_RMS).abs() < 2e-3, "{audible}");

        let mut muted = project.clone();
        muted.tracks[0].strip.settings.mute = true;
        assert_eq!(level(&muted, &[NodeKey::Clips(a)]), 0.0);
        assert!(level(&muted, &[NodeKey::Clips(b)]) > 0.1);

        // Soloing `a` silences `b`, pre-fader send included.
        let mut soloed = project.clone();
        soloed.tracks[0].strip.settings.solo = true;
        assert!(level(&soloed, &[NodeKey::Clips(a)]) > 0.1);
        assert_eq!(level(&soloed, &[NodeKey::Clips(b)]), 0.0);
    }

    #[test]
    fn compile_instrument_and_inserts_are_placeholders() {
        let mut ed = Editor::new(Project::new("p"));
        let id: TrackId = ed.alloc_id().unwrap();
        let synth: ow_model::DeviceId = ed.alloc_id().unwrap();
        let fx: ow_model::DeviceId = ed.alloc_id().unwrap();
        let device = |id, kind: &str| Device {
            id,
            kind: kind.into(),
            name: kind.into(),
            enabled: true,
            params: Default::default(),
            state: vec![],
            sidechain: None,
        };
        ed.apply(EditCommand::InsertTrack {
            index: 0,
            bundle: Box::new(Owned {
                item: Track {
                    id,
                    name: "Keys".into(),
                    color: 0,
                    kind: TrackKind::Midi,
                    strip: Strip {
                        inserts: vec![fx],
                        ..Strip::default()
                    },
                    instrument: Some(synth),
                },
                clips: vec![],
                devices: vec![device(synth, "builtin.synth"), device(fx, "builtin.eq")],
                lanes: vec![],
            }),
        })
        .unwrap();
        // Source tone -> insert -> fader -> master: unity.
        let l = level(ed.project(), &[NodeKey::Device(synth)]);
        assert!((l - SOURCE_RMS).abs() < 1e-3, "{l}");
        let compiled = compile_project(ed.project(), RATE, 256).unwrap();
        assert!(compiled.nodes.get(NodeKey::Device(fx)).is_some());
    }

    #[test]
    fn compile_is_deterministic() {
        let mut ed = Editor::new(Project::new("p"));
        let m = media(&mut ed);
        let drums = bus(&mut ed, db(-3.0));
        for i in 0..4 {
            let settings = StripSettings {
                output: if i % 2 == 0 {
                    Output::Bus(drums)
                } else {
                    Output::Master
                },
                ..db(-f64::from(i))
            };
            audio_track(&mut ed, m, settings);
        }
        let a = compile_project(ed.project(), RATE, 256).unwrap();
        let b = compile_project(ed.project(), RATE, 256).unwrap();
        assert_eq!(a.nodes, b.nodes);
        let render = |c: Compiled| {
            let mut r = OfflineRenderer::new(c.plan, 2);
            for (key, node) in c.nodes.iter() {
                if let NodeKey::Clips(_) = key {
                    r.send(Message::SetParam {
                        node,
                        param: TONE_PARAM_ON,
                        value: 1.0,
                    });
                }
            }
            r.render(8192, 100)
        };
        assert_eq!(render(a), render(b));
    }

    #[test]
    fn compile_node_identity_is_stable_across_recompiles() {
        let mut ed = Editor::new(Project::new("p"));
        let m = media(&mut ed);
        let aux = bus(&mut ed, db(0.0));
        let first = audio_track(&mut ed, m, db(0.0));
        let second = audio_track(&mut ed, m, db(0.0));
        ed.apply(EditCommand::SetSends {
            target: MixerRef::Track(second),
            sends: vec![AuxSend {
                target: aux,
                level_db: -3.0,
                pre_fader: false,
            }],
        })
        .unwrap();
        let send_key = NodeKey::Send {
            from: MixerRef::Track(second),
            to: aux,
            nth: 0,
        };
        let before = compile_project(ed.project(), RATE, 256).unwrap().nodes;
        assert!(before.get(send_key).is_some());

        // Appending a track keeps every existing node id and only adds nodes.
        let third = audio_track(&mut ed, m, db(0.0));
        let after_append = compile_project(ed.project(), RATE, 256).unwrap().nodes;
        let diff = before.diff(&after_append);
        assert!(diff.removed.is_empty());
        assert_eq!(diff.carried.len(), before.len());
        // Strip nodes keep their ids; the send, created after all strips, shifts.
        for carry in &diff.carried {
            if carry.key == send_key {
                assert_ne!(carry.old, carry.new, "the send's id moved");
            } else {
                assert_eq!(carry.old, carry.new, "{:?}", carry.key);
            }
        }
        assert_eq!(
            after_append.get(send_key),
            diff.carried
                .iter()
                .find(|c| c.key == send_key)
                .map(|c| c.new)
        );
        let added: Vec<NodeKey> = diff.added.iter().map(|(k, _)| *k).collect();
        assert!(added.contains(&NodeKey::Clips(third)));
        assert!(added.contains(&NodeKey::Fader(MixerRef::Track(third))));

        // Removing the first track: its nodes go, the others carry over by key even though
        // their node ids shift.
        ed.apply(EditCommand::RemoveTrack { id: first }).unwrap();
        let after_remove = compile_project(ed.project(), RATE, 256).unwrap().nodes;
        let diff = after_append.diff(&after_remove);
        let removed: Vec<NodeKey> = diff.removed.iter().map(|(k, _)| *k).collect();
        assert!(removed.contains(&NodeKey::Clips(first)));
        assert!(removed.contains(&NodeKey::Fader(MixerRef::Track(first))));
        assert!(diff.added.is_empty());
        let carried = |key| diff.carried.iter().find(|c| c.key == key).copied();
        assert!(carried(send_key).is_some(), "the send carries over by key");
        let moved = carried(NodeKey::Clips(second)).expect("second track carries over");
        assert_ne!(moved.old, moved.new, "ids shifted, keys did not");
        assert_eq!(after_remove.get(NodeKey::Clips(second)), Some(moved.new));
        assert_eq!(after_append.get(NodeKey::Clips(second)), Some(moved.old));
        assert!(carried(NodeKey::Fader(MixerRef::Master)).is_some());
    }

    #[test]
    fn compile_fader_changes_apply_without_a_recompile() {
        let mut ed = Editor::new(Project::new("p"));
        let m = media(&mut ed);
        let t = audio_track(&mut ed, m, db(0.0));
        let compiled = compile_project(ed.project(), RATE, 256).unwrap();
        let tone = compiled.nodes.get(NodeKey::Clips(t)).unwrap();
        let fader = compiled.nodes.fader(MixerRef::Track(t)).unwrap();
        let mut renderer = OfflineRenderer::new(compiled.plan, 2);
        renderer.send(Message::SetParam {
            node: tone,
            param: TONE_PARAM_ON,
            value: 1.0,
        });
        renderer.render(8192, 256);

        let mut muted = ed.project().clone();
        muted.tracks[0].strip.settings.mute = true;
        renderer.send(Message::SetParam {
            node: fader,
            param: GAIN_PARAM,
            value: fader_gain(&muted, MixerRef::Track(t)),
        });
        renderer.render(1024, 256); // the gain ramps over one block
        let out = renderer.render(2048, 256);
        assert!(out.iter().all(|s| s.abs() < 1e-6));
    }

    #[test]
    fn compile_metered_faders_publish_peak_and_rms_to_their_slots() {
        let mut ed = Editor::new(Project::new("p"));
        let m = media(&mut ed);
        let a = audio_track(&mut ed, m, db(0.0));
        let b = audio_track(&mut ed, m, db(-6.0206));
        let bank = Arc::new(TelemetryBank::new());
        let compiled = compile_project_metered(ed.project(), RATE, 256, Some(&bank)).unwrap();
        let mut renderer = OfflineRenderer::new(compiled.plan, 2);
        for track in [a, b] {
            renderer.send(Message::SetParam {
                node: compiled.nodes.get(NodeKey::Clips(track)).unwrap(),
                param: TONE_PARAM_ON,
                value: 1.0,
            });
        }
        renderer.render(FRAMES, 256);
        let slot = |strip| ow_ipc::meter_slot(ed.project(), strip).unwrap();
        let (peak_a, rms_a) = bank.take_meter(slot(MixerRef::Track(a))).unwrap();
        let (peak_b, rms_b) = bank.take_meter(slot(MixerRef::Track(b))).unwrap();
        let (peak_master, _) = bank.take_meter(slot(MixerRef::Master)).unwrap();
        assert!((peak_a[0] - 0.2).abs() < 0.01, "{peak_a:?}");
        assert!((rms_a[0] - SOURCE_RMS).abs() < 2e-3, "{rms_a:?}");
        assert!((peak_b[1] - 0.1).abs() < 0.01, "{peak_b:?}");
        assert!((rms_b[0] - SOURCE_RMS / 2.0).abs() < 2e-3, "{rms_b:?}");
        assert!(peak_master[0] > peak_a[0], "the master carries both tracks");
        // Unmetered compiles leave the bank alone.
        let silent = TelemetryBank::new();
        let _ = compile_project(ed.project(), RATE, 256).unwrap();
        assert_eq!(silent.take_meter(0).unwrap().0, [0.0, 0.0]);
    }

    #[test]
    fn compile_rejects_an_invalid_project() {
        let mut project = Project::new("bad");
        project.settings.sample_rate = 0;
        assert!(matches!(
            compile_project(&project, RATE, 256),
            Err(CompileError::Model(_))
        ));
        assert!(matches!(
            compile_project(&Project::new("ok"), 0, 256),
            Err(CompileError::Graph(GraphError::InvalidFormat))
        ));
    }
}
