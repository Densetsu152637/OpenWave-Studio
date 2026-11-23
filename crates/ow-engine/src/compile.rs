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
//! * sends are [`GainNode`]s tapped pre- or post-fader that feed the target bus (a pre-fader tap
//!   is after the inserts and the pan stage; a bus without a source or inserts gets a unity input
//!   stage, [`NodeKey::Input`], so its pre-fader sends have something to tap);
//! * pan is a [`PanNode`] between the inserts and the fader ([`NodeKey::Pan`]), present when its
//!   gains are not both 1; see [`pan_gains`] for the law. Master and bus strips carry stereo and
//!   use balance; a track whose source is mono (mono audio media, or an instrument, which
//!   plays the same signal on both sides) uses the constant-power law with -3 dB at the centre;
//! * audio clips play: a track with audio clips gets a stereo [`GainNode`] as its source (key
//!   [`NodeKey::Clips`]) that sums one [`ClipSourceNode`] per clip (key [`NodeKey::Clip`]), each
//!   playing its media from memory at the clip's timeline position with the clip's source
//!   offset and gain (see [`CompileContext`]); mono media feeds both sides. Muted clips and
//!   clips whose media cannot be loaded get no node (silence), and the latter are listed in
//!   [`Compiled::missing_media`]. Fades are not applied yet;
//! * a MIDI track's instrument is a stereo [`DeviceNode`] (key [`NodeKey::Device`]) built from
//!   the `ow-dsp` registry entry for the device's `kind` and the device's parameters. It plays
//!   the track's unmuted MIDI clips: the node holds a
//!   [`MidiClipPlayer`](crate::clips::midi::MidiClipPlayer) that turns the clips' notes into
//!   sample-accurate events inside `process`. An instrument of an unknown kind, or one that is
//!   disabled, is silent;
//! * inserts are stereo-in, stereo-out [`DeviceNode`]s in chain order after the source, created
//!   from the registry the same way. A disabled insert, or one whose kind the registry does not
//!   know (no effect is built in yet), is a unity [`GainNode`] pass-through under the same key;
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

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use ow_dsp::DeviceInfo;
use ow_media::MemorySource;
use ow_model::{
    AuxSend, BusId, Clip, ClipContent, ClipId, Device, DeviceId, MediaId, MixerRef, Output,
    Project, Strip, Track, TrackId, ValidationError,
};

use ow_ipc::TelemetryBank;

use crate::clips::SourceCache;
use crate::clips::midi::{MidiClipPlayer, schedule};
use crate::device::DeviceNode;
use crate::graph::clip::{ClipPlacement, ClipSourceNode};
use crate::graph::nodes::{GainNode, PanNode};
use crate::{Graph, GraphError, NodeId, RenderPlan, Timeline};

/// What the compiler needs besides the project to play its audio clips.
#[derive(Debug, Clone, Default)]
pub struct CompileContext {
    /// The folder `MediaItem::path` is relative to: the project bundle, or the temporary media
    /// folder of a project that was never saved.
    pub media_root: PathBuf,
    /// Media already loaded (and resampled to the engine rate), shared by every compile with
    /// this context.
    pub sources: SourceCache,
}

impl CompileContext {
    /// A context over `media_root` with an empty cache.
    pub fn new(media_root: impl Into<PathBuf>) -> Self {
        Self {
            media_root: media_root.into(),
            sources: SourceCache::new(),
        }
    }
}

/// Identity of a compiled node in model terms: stable across recompiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeKey {
    /// The sum of a track's audio clips, the track's source.
    Clips(TrackId),
    /// One audio clip's player (absent for a muted clip or one whose media is missing).
    Clip(ClipId),
    /// A device: a MIDI track's instrument or an insert effect.
    Device(DeviceId),
    /// A strip's fader; the master's is the output gain.
    Fader(MixerRef),
    /// A strip's pan or balance stage (absent when it would be unity).
    Pan(MixerRef),
    /// The unity input stage of a bus or master that has no source or inserts but needs a node
    /// before its pan stage or pre-fader sends.
    Input(MixerRef),
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
    /// Media items some unmuted clip plays whose file could not be loaded (missing, unreadable
    /// or too large), in ascending id order. Those clips are silent; this is a warning, not an
    /// error.
    pub missing_media: Vec<MediaId>,
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

/// Left and right gains of a strip's pan stage for `pan` (-1 left to 1 right; out-of-range and
/// non-finite values are limited and read as centre).
///
/// With a `mono` source the law is constant power: `left = cos(t)`, `right = sin(t)` for
/// `t = (pan + 1) * pi / 4`, so both sides are -3.01 dB at the centre and the summed power is
/// constant. A stereo source gets balance: the side away from the pan direction is attenuated by
/// `cos(|pan| * pi / 2)` (silent at full pan) and the other side stays at unity, so the centre
/// is unity.
pub fn pan_gains(pan: f64, mono: bool) -> [f32; 2] {
    let pan = if pan.is_finite() {
        pan.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    if mono {
        let angle = (pan + 1.0) * std::f64::consts::FRAC_PI_4;
        [angle.cos() as f32, angle.sin() as f32]
    } else {
        let side = |amount: f64| (amount * std::f64::consts::FRAC_PI_2).cos().max(0.0) as f32;
        [
            if pan > 0.0 { side(pan) } else { 1.0 },
            if pan < 0.0 { side(-pan) } else { 1.0 },
        ]
    }
}

/// What `build_strip` leaves for wiring.
struct StripNodes {
    /// First node of the strip's chain, which incoming signals connect to (stereo input).
    entry: NodeId,
    /// The stereo signal before the fader (pre-fader sends tap here); `None` when the strip has
    /// no source and no inserts, in which case pre-fader sends tap the fader instead (a bus
    /// without inserts has no node before its fader until the mixer stage adds an input stage).
    pre: Option<NodeId>,
    /// The fader.
    fader: NodeId,
}

/// Connects both channels of the stereo node `from` to those of `to`. Every compiled strip node
/// is stereo (mono media is widened inside the clip sum), so this is the only wiring there is.
fn connect_stereo(graph: &mut Graph, from: NodeId, to: NodeId) -> Result<(), GraphError> {
    graph.connect(from, 0, to, 0)?;
    graph.connect(from, 1, to, 1)
}

/// Compiles `project` for `sample_rate` and blocks of up to `max_block` frames.
///
/// Allocates freely and may read and resample media files (through `ctx`): run it on the
/// control thread. A file that cannot be loaded is reported in [`Compiled::missing_media`],
/// never as an error.
pub fn compile_project(
    project: &Project,
    ctx: &CompileContext,
    sample_rate: u32,
    max_block: usize,
) -> Result<Compiled, CompileError> {
    compile_project_metered(project, ctx, sample_rate, max_block, None)
}

/// Like [`compile_project`] with the plan's output taken from the fader of `tap` instead of the
/// master's: what `strip` sounds like after its fader, for analysis. The rest of the project is
/// compiled as usual. A strip the project does not have is a model error.
pub fn compile_project_tap(
    project: &Project,
    ctx: &CompileContext,
    sample_rate: u32,
    max_block: usize,
    tap: MixerRef,
) -> Result<Compiled, CompileError> {
    compile_inner(project, ctx, sample_rate, max_block, None, tap)
}

/// Like [`compile_project`], and when `meters` is given every strip's fader publishes its peak
/// and RMS to the bank slot [`ow_ipc::meter_slot`] assigns it (the UI reads them through the
/// telemetry channel). Strips beyond the bank's slots are not metered.
pub fn compile_project_metered(
    project: &Project,
    ctx: &CompileContext,
    sample_rate: u32,
    max_block: usize,
    meters: Option<&Arc<TelemetryBank>>,
) -> Result<Compiled, CompileError> {
    compile_inner(
        project,
        ctx,
        sample_rate,
        max_block,
        meters,
        MixerRef::Master,
    )
}

fn compile_inner(
    project: &Project,
    ctx: &CompileContext,
    sample_rate: u32,
    max_block: usize,
    meters: Option<&Arc<TelemetryBank>>,
    tap: MixerRef,
) -> Result<Compiled, CompileError> {
    project.validate().map_err(CompileError::Model)?;
    if project.strip(tap).is_none() {
        return Err(CompileError::Model(ValidationError {
            path: "tap".into(),
            message: format!("there is no strip {tap:?}"),
        }));
    }
    let timeline = Timeline::new(
        project.tempo_map.clone(),
        project.time_signature_map.clone(),
    );
    let mut missing = BTreeSet::new();
    let mut clips = ClipEnv {
        ctx,
        timeline: &timeline,
        sample_rate,
        max_block,
        missing: &mut missing,
    };

    let shared = Shared {
        meters,
        sample_rate,
        max_block,
    };
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
        shared,
        None,
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
            let nodes = build_strip(
                &mut graph,
                &mut add,
                project,
                *target,
                strip,
                shared,
                Some(&mut clips),
            );
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
    graph.set_master(built[&tap].fader)?;

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
            let post = nodes_of.fader;
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
    Ok(Compiled {
        plan,
        timeline,
        nodes,
        missing_media: missing.into_iter().collect(),
    })
}

/// What every strip build shares.
#[derive(Clone, Copy)]
struct Shared<'a> {
    meters: Option<&'a Arc<TelemetryBank>>,
    sample_rate: u32,
    max_block: usize,
}

/// What `build_strip` needs to compile a track's audio clips.
struct ClipEnv<'a> {
    ctx: &'a CompileContext,
    timeline: &'a Timeline,
    sample_rate: u32,
    max_block: usize,
    /// Media that could not be loaded, collected across tracks.
    missing: &'a mut BTreeSet<MediaId>,
}

/// Adds a [`ClipSourceNode`] for `clip` and wires it into the track's summing node `sum`.
/// Adds nothing when the clip is silent; a media file that cannot be loaded is recorded in
/// `env.missing`.
fn add_audio_clip(
    graph: &mut Graph,
    add: &mut AddNode<'_>,
    env: &mut ClipEnv<'_>,
    project: &Project,
    clip: &Clip,
    sum: NodeId,
) -> bool {
    let ClipContent::Audio {
        media,
        source_offset,
        gain_db,
        ..
    } = &clip.content
    else {
        return false;
    };
    // The model guarantees the media exists (`validate`).
    let Some(item) = project.media_pool.get(media).filter(|_| !clip.muted) else {
        return false;
    };
    let rate = env.sample_rate;
    let Ok(loaded) = env
        .ctx
        .sources
        .load(&env.ctx.media_root.join(&item.path), rate)
    else {
        env.missing.insert(*media);
        return false;
    };
    let channels = loaded.pcm.channels.len();
    let source_frames = loaded.pcm.frames() as u64;
    if channels == 0 || source_frames == 0 {
        return false;
    }
    // Placement in timeline samples. Source offsets count frames of the media file, which
    // `load` resampled to the engine rate, so they scale by the same ratio.
    let start = env.timeline.beat_to_sample_index(clip.start, rate);
    let end = env
        .timeline
        .beat_to_sample_index(clip.start + clip.length, rate);
    let scale = f64::from(rate) / f64::from(loaded.source_rate.max(1));
    let offset = (*source_offset as f64 * scale).round();
    // Past the end plays silence either way; clamping keeps position arithmetic far from
    // overflow on the audio thread.
    let offset = if offset >= source_frames as f64 {
        source_frames
    } else {
        offset as u64
    };
    let placement = ClipPlacement {
        start,
        length: end.saturating_sub(start),
        source_offset: offset,
        gain: db_to_linear(*gain_db),
    };
    let source = Box::new(MemorySource::new(loaded.pcm));
    let node = add(
        graph,
        NodeKey::Clip(clip.id),
        Box::new(ClipSourceNode::new(source, vec![placement])),
    );
    // Both ends were just added with these channel counts, so connecting cannot fail.
    let right = usize::from(channels > 1);
    let wired = graph
        .connect(node, 0, sum, 0)
        .and_then(|()| graph.connect(node, right, sum, 1));
    debug_assert!(wired.is_ok());
    channels > 1
}

type AddNode<'a> = dyn FnMut(&mut Graph, NodeKey, Box<dyn crate::Node>) -> NodeId + 'a;

/// The device `device` of the model as an `ow-dsp` instance: every parameter at the value the
/// model holds (clamped to the descriptor's range) or the default.
fn make_device(
    info: &'static DeviceInfo,
    device: &Device,
    sample_rate: u32,
    max_block: usize,
) -> Box<dyn ow_dsp::Device> {
    let mut made = info.create(sample_rate as f32, max_block);
    for param in info.params {
        if let Some(value) = device.params.get(&param.id) {
            made.set_param(param.id, param.clamp(*value));
        }
    }
    made
}

/// Adds the instrument node of a MIDI track: the registry's instrument for the device's kind
/// with the track's MIDI clips as its note source. `None` (a silent track) when the device is
/// disabled, missing or of a kind that is not a built-in instrument.
fn add_instrument(
    graph: &mut Graph,
    add: &mut AddNode<'_>,
    env: &ClipEnv<'_>,
    project: &Project,
    track: &Track,
    instrument: DeviceId,
) -> Option<NodeId> {
    let device = project.devices.get(&instrument).filter(|d| d.enabled)?;
    let info = ow_dsp::find(&device.kind).filter(|info| info.instrument)?;
    let events = schedule(
        project.clips.values().filter(|c| c.track == track.id),
        env.timeline,
        env.sample_rate,
    );
    let node = DeviceNode::instrument(
        info,
        make_device(info, device, env.sample_rate, env.max_block),
        MidiClipPlayer::new(events),
    );
    Some(add(graph, NodeKey::Device(instrument), Box::new(node)))
}

/// Adds the node of one insert: the built-in effect when there is one and it is enabled,
/// otherwise a unity pass-through (bypass, or a kind this build does not have).
fn add_insert(
    graph: &mut Graph,
    add: &mut AddNode<'_>,
    project: &Project,
    id: DeviceId,
    sample_rate: u32,
    max_block: usize,
) -> NodeId {
    let effect = project
        .devices
        .get(&id)
        .filter(|d| d.enabled)
        .and_then(|d| Some((d, ow_dsp::find(&d.kind).filter(|info| !info.instrument)?)));
    let node: Box<dyn crate::Node> = match effect {
        Some((device, info)) => Box::new(DeviceNode::effect(
            info,
            make_device(info, device, sample_rate, max_block),
        )),
        None => Box::new(GainNode::new(2, 1.0)),
    };
    add(graph, NodeKey::Device(id), node)
}

/// Adds the nodes of one strip and wires source, inserts and fader together.
fn build_strip(
    graph: &mut Graph,
    add: &mut AddNode<'_>,
    project: &Project,
    target: MixerRef,
    strip: &Strip,
    shared: Shared<'_>,
    clips: Option<&mut ClipEnv<'_>>,
) -> StripNodes {
    let Shared {
        meters,
        sample_rate,
        max_block,
    } = shared;
    // Source: the track's audio clips, or its instrument playing its MIDI clips. Both are
    // stereo nodes.
    let mut source: Option<NodeId> = None;
    // Buses and the master carry stereo; a track is mono until some audio clip says otherwise.
    let mut mono_source = matches!(target, MixerRef::Track(_));
    if let MixerRef::Track(id) = target
        && let Some(track) = project.track(id)
        && let Some(env) = clips
    {
        if let Some(instrument) = track.instrument {
            source = add_instrument(graph, add, env, project, track, instrument);
        } else {
            let own: Vec<&Clip> = project
                .clips
                .values()
                .filter(|c| c.track == id && matches!(c.content, ClipContent::Audio { .. }))
                .collect();
            if !own.is_empty() {
                let sum = add(graph, NodeKey::Clips(id), Box::new(GainNode::new(2, 1.0)));
                let mut stereo = false;
                for clip in own {
                    stereo |= add_audio_clip(graph, add, env, project, clip, sum);
                }
                mono_source = !stereo;
                source = Some(sum);
            }
        }
    }

    // The stage that follows the inserts: pan or balance, when it would change the signal.
    let pan = pan_gains(strip.settings.pan, mono_source);
    let needs_pan = pan != [1.0, 1.0];
    let pre_sends = strip.sends.iter().any(|send| send.pre_fader);

    // Inserts in chain order, each fed by the node before it.
    let mut previous = source;
    let mut entry: Option<NodeId> = None;
    let mut wire: Vec<(NodeId, NodeId)> = Vec::new();
    // A bus or master with neither a source nor inserts has no node to receive what routes into
    // it ahead of its pan stage or pre-fader sends: give it a unity one.
    if !matches!(target, MixerRef::Track(_))
        && source.is_none()
        && strip.inserts.is_empty()
        && (needs_pan || pre_sends)
    {
        let node = add(
            graph,
            NodeKey::Input(target),
            Box::new(GainNode::new(2, 1.0)),
        );
        entry = Some(node);
        previous = Some(node);
    }
    for device in &strip.inserts {
        let node = add_insert(graph, add, project, *device, sample_rate, max_block);
        entry.get_or_insert(node);
        if let Some(from) = previous {
            wire.push((from, node));
        }
        previous = Some(node);
    }

    if needs_pan && let Some(from) = previous {
        let node = add(graph, NodeKey::Pan(target), Box::new(PanNode::new(pan)));
        entry.get_or_insert(node);
        wire.push((from, node));
        previous = Some(node);
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
    use crate::graph::nodes::GAIN_PARAM;
    use crate::{Message, OfflineRenderer};

    const RATE: u32 = 48_000;
    const FRAMES: usize = 24_000;

    /// A media folder holding `audio/m.wav`: a mono 440 Hz sine of amplitude 0.2, one second.
    fn media_dir() -> (tempfile::TempDir, CompileContext) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("audio")).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(dir.path().join("audio/m.wav"), spec).unwrap();
        for i in 0..RATE {
            let phase = std::f64::consts::TAU * 440.0 * f64::from(i) / f64::from(RATE);
            writer.write_sample((0.2 * phase.sin()) as f32).unwrap();
        }
        writer.finalize().unwrap();
        let ctx = CompileContext::new(dir.path());
        (dir, ctx)
    }

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
                frames: u64::from(RATE),
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

    /// Plays the project; returns the RMS of the left channel over the last 4096 frames.
    fn project_level(project: &Project, ctx: &CompileContext) -> f32 {
        let compiled = compile_project(project, ctx, RATE, 256).unwrap();
        let mut renderer = OfflineRenderer::new(compiled.plan, 2);
        assert!(renderer.send(Message::Transport(crate::TransportCommand::Play)));
        let out = renderer.render(FRAMES, 256);
        let tail = &out[(FRAMES - 4096) * 2..];
        let sum: f32 = tail.chunks(2).map(|f| f[0] * f[0]).sum();
        (sum / 4096.0).sqrt()
    }

    /// Like [`project_level`] with only the clips of the `audible` tracks playing (the others are
    /// muted), so one track's contribution can be measured.
    fn level(project: &Project, ctx: &CompileContext, audible: &[TrackId]) -> f32 {
        let mut project = project.clone();
        for clip in project.clips.values_mut() {
            clip.muted |= !audible.contains(&clip.track);
        }
        project_level(&project, ctx)
    }

    /// RMS of the 0.2-amplitude sine source on a centred track: the sine's RMS, times the -3 dB
    /// the constant-power pan law leaves a mono source at the centre.
    const SOURCE_RMS: f32 = 0.2 * std::f32::consts::FRAC_1_SQRT_2 * std::f32::consts::FRAC_1_SQRT_2;

    #[test]
    fn compile_empty_project_renders_silence_with_the_models_timeline() {
        let project = Project::new("empty");
        let compiled = compile_project(&project, &CompileContext::default(), RATE, 256).unwrap();
        assert_eq!(compiled.timeline.tempo(), &project.tempo_map);
        assert_eq!(compiled.timeline.signatures(), &project.time_signature_map);
        assert_eq!(compiled.nodes.len(), 1, "only the master fader");
        assert_eq!(compiled.plan.master_channels(), 2);
        let mut renderer = OfflineRenderer::new(compiled.plan, 2);
        assert!(renderer.render(2048, 256).iter().all(|s| *s == 0.0));
    }

    #[test]
    fn compile_track_gain_reaches_the_master() {
        let (_dir, ctx) = media_dir();
        let mut ed = Editor::new(Project::new("p"));
        let m = media(&mut ed);
        let a = audio_track(&mut ed, m, db(0.0));
        let b = audio_track(&mut ed, m, db(-6.0206));
        let only_a = level(ed.project(), &ctx, &[a]);
        let only_b = level(ed.project(), &ctx, &[b]);
        assert!((only_a - SOURCE_RMS).abs() < 1e-3, "{only_a}");
        assert!((only_b - SOURCE_RMS / 2.0).abs() < 1e-3, "{only_b}");
        // Without clips a track has no source node at all.
        let empty = compile_project(&Project::new("x"), &ctx, RATE, 64).unwrap();
        assert!(empty.nodes.get(NodeKey::Clips(a)).is_none());
    }

    #[test]
    fn compile_mute_solo_and_phase() {
        let (_dir, ctx) = media_dir();
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
        assert_eq!(level(ed.project(), &ctx, &[a]), 0.0);
        assert!(level(ed.project(), &ctx, &[b]) > 0.05);

        // Soloing `b` silences `a` and `c` unless solo-safe.
        let mut soloed = ed.project().clone();
        soloed.tracks[1].strip.settings.solo = true;
        assert!(level(&soloed, &ctx, &[b]) > 0.05);
        assert!(level(&soloed, &ctx, &[c]) > 0.05, "solo-safe track stays");
        soloed.tracks[2].strip.settings.solo_safe = false;
        assert_eq!(level(&soloed, &ctx, &[c]), 0.0);

        // Polarity inversion cancels an identical track.
        let mut inverted = ed.project().clone();
        inverted.tracks[0].strip.settings.mute = false;
        inverted.tracks[1].strip.settings.invert_phase = true;
        assert!(level(&inverted, &ctx, &[a, b]) < 1e-3);

        assert_eq!(fader_gain(&soloed, MixerRef::Track(a)), 0.0);
        assert_eq!(fader_gain(&soloed, MixerRef::Track(b)), 1.0);
        assert_eq!(fader_gain(&inverted, MixerRef::Track(b)), -1.0);
    }

    #[test]
    fn compile_bus_routing_and_sends() {
        let (_dir, ctx) = media_dir();
        let mut ed = Editor::new(Project::new("p"));
        let m = media(&mut ed);
        let drums = bus(&mut ed, db(-6.0206));
        let to_bus = StripSettings {
            output: Output::Bus(drums),
            ..db(0.0)
        };
        let track = audio_track(&mut ed, m, to_bus);
        let key = [track];
        // Routed through a -6 dB bus.
        let routed = level(ed.project(), &ctx, &key);
        assert!((routed - SOURCE_RMS / 2.0).abs() < 1e-3, "{routed}");

        // Direct to master plus a post-fader -6 dB send to the bus: 1 + 0.5 x 0.5.
        let mut with_send = ed.project().clone();
        with_send.tracks[0].strip.settings.output = Output::Master;
        with_send.tracks[0].strip.sends = vec![AuxSend {
            target: drums,
            level_db: -6.0206,
            pre_fader: false,
        }];
        let sent = level(&with_send, &ctx, &key);
        assert!((sent - SOURCE_RMS * 1.25).abs() < 2e-3, "{sent}");

        // A pre-fader send ignores the track fader: fader nearly off, send still audible.
        with_send.tracks[0].strip.settings.gain_db = -100.0;
        with_send.tracks[0].strip.sends[0].pre_fader = true;
        let pre = level(&with_send, &ctx, &key);
        assert!((pre - SOURCE_RMS * 0.25).abs() < 2e-3, "{pre}");
        let compiled = compile_project(&with_send, &ctx, RATE, 256).unwrap();
        let send_key = NodeKey::Send {
            from: MixerRef::Track(track),
            to: drums,
            nth: 0,
        };
        assert!(compiled.nodes.get(send_key).is_some());
    }

    #[test]
    fn compile_pre_fader_sends_respect_mute_and_solo() {
        let (_dir, ctx) = media_dir();
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
        let audible = level(&project, &ctx, &[a]);
        assert!((audible - SOURCE_RMS).abs() < 2e-3, "{audible}");

        let mut muted = project.clone();
        muted.tracks[0].strip.settings.mute = true;
        assert_eq!(level(&muted, &ctx, &[a]), 0.0);
        assert!(level(&muted, &ctx, &[b]) > 0.05);

        // Soloing `a` silences `b`, pre-fader send included.
        let mut soloed = project.clone();
        soloed.tracks[0].strip.settings.solo = true;
        assert!(level(&soloed, &ctx, &[a]) > 0.05);
        assert_eq!(level(&soloed, &ctx, &[b]), 0.0);
    }

    /// A MIDI track with a one-bar clip holding a long note, the given instrument kind and one
    /// insert of `insert_kind`; returns the project and the two device ids.
    fn midi_project(instrument_kind: &str, insert_kind: &str) -> (Project, DeviceId, DeviceId) {
        let mut ed = Editor::new(Project::new("p"));
        let id: TrackId = ed.alloc_id().unwrap();
        let synth: DeviceId = ed.alloc_id().unwrap();
        let fx: DeviceId = ed.alloc_id().unwrap();
        let clip = ed.alloc_id().unwrap();
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
                clips: vec![Clip {
                    id: clip,
                    track: id,
                    name: "notes".into(),
                    start: 0.0,
                    length: 8.0,
                    muted: false,
                    content: ClipContent::Midi {
                        notes: vec![ow_model::Note {
                            start: 0.0,
                            length: 8.0,
                            key: 69,
                            velocity: 100,
                        }],
                        controllers: vec![],
                    },
                }],
                devices: vec![device(synth, instrument_kind), device(fx, insert_kind)],
                lanes: vec![],
            }),
        })
        .unwrap();
        (ed.project().clone(), synth, fx)
    }

    #[test]
    fn compile_midi_track_plays_its_clips_through_the_instrument_and_inserts() {
        let ctx = CompileContext::default();
        let (project, synth, fx) = midi_project("builtin.synth", "builtin.nothing");
        let played = project_level(&project, &ctx);
        assert!(played > 0.01, "the synth sounds: {played}");
        let compiled = compile_project(&project, &ctx, RATE, 256).unwrap();
        assert!(compiled.nodes.get(NodeKey::Device(synth)).is_some());
        // An insert of a kind this build does not have is a unity pass-through under its key.
        assert!(compiled.nodes.get(NodeKey::Device(fx)).is_some());
        let mut without = project.clone();
        without.tracks[0].strip.inserts.clear();
        without.devices.remove(&fx);
        assert_eq!(project_level(&without, &ctx), played);

        // A muted clip, a disabled instrument and an instrument of an unknown kind are silent.
        let mut muted = project.clone();
        muted.clips.values_mut().for_each(|c| c.muted = true);
        assert_eq!(project_level(&muted, &ctx), 0.0);
        let mut disabled = project.clone();
        disabled.devices.get_mut(&synth).unwrap().enabled = false;
        assert_eq!(project_level(&disabled, &ctx), 0.0);
        let (unknown, ..) = midi_project("vst3:something", "builtin.nothing");
        assert_eq!(project_level(&unknown, &ctx), 0.0);
        let compiled = compile_project(&unknown, &ctx, RATE, 256).unwrap();
        assert!(
            compiled
                .nodes
                .get(NodeKey::Fader(MixerRef::Track(unknown.tracks[0].id)))
                .is_some()
        );
    }

    #[test]
    fn compile_applies_the_models_device_parameters() {
        let ctx = CompileContext::default();
        let (mut project, synth, _) = midi_project("builtin.synth", "builtin.nothing");
        let loud = project_level(&project, &ctx);
        // Gain (parameter 7, default -9 dB) down another 6 dB halves the level; an out-of-range
        // value is clamped to the descriptor's range.
        project
            .devices
            .get_mut(&synth)
            .unwrap()
            .params
            .insert(7, -15.0206);
        let quiet = project_level(&project, &ctx);
        assert!((quiet / loud - 0.5).abs() < 0.01, "{quiet} vs {loud}");
        project
            .devices
            .get_mut(&synth)
            .unwrap()
            .params
            .insert(7, -1000.0);
        assert!(project_level(&project, &ctx) < 1e-3, "clamped to -60 dB");
    }

    #[test]
    fn compile_is_deterministic() {
        let (_dir, ctx) = media_dir();
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
        let a = compile_project(ed.project(), &ctx, RATE, 256).unwrap();
        let b = compile_project(ed.project(), &ctx, RATE, 256).unwrap();
        assert_eq!(a.nodes, b.nodes);
        let render = |c: Compiled| {
            let mut r = OfflineRenderer::new(c.plan, 2);
            r.send(Message::Transport(crate::TransportCommand::Play));
            r.render(8192, 100)
        };
        let (a, b) = (render(a), render(b));
        assert!(a.iter().any(|s| s.abs() > 0.1), "the clips play");
        assert_eq!(a, b);
    }

    #[test]
    fn compile_node_identity_is_stable_across_recompiles() {
        let (_dir, ctx) = media_dir();
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
        let before = compile_project(ed.project(), &ctx, RATE, 256)
            .unwrap()
            .nodes;
        assert!(before.get(send_key).is_some());

        // Appending a track keeps every existing node id and only adds nodes.
        let third = audio_track(&mut ed, m, db(0.0));
        let after_append = compile_project(ed.project(), &ctx, RATE, 256)
            .unwrap()
            .nodes;
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
        let after_remove = compile_project(ed.project(), &ctx, RATE, 256)
            .unwrap()
            .nodes;
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
        let (_dir, ctx) = media_dir();
        let mut ed = Editor::new(Project::new("p"));
        let m = media(&mut ed);
        let t = audio_track(&mut ed, m, db(0.0));
        let compiled = compile_project(ed.project(), &ctx, RATE, 256).unwrap();
        let fader = compiled.nodes.fader(MixerRef::Track(t)).unwrap();
        let mut renderer = OfflineRenderer::new(compiled.plan, 2);
        renderer.send(Message::Transport(crate::TransportCommand::Play));
        let before = renderer.render(8192, 256);
        assert!(before.iter().any(|s| s.abs() > 0.1), "the clip plays");

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
        let (_dir, ctx) = media_dir();
        let mut ed = Editor::new(Project::new("p"));
        let m = media(&mut ed);
        let a = audio_track(&mut ed, m, db(0.0));
        let b = audio_track(&mut ed, m, db(-6.0206));
        let bank = Arc::new(TelemetryBank::new());
        let compiled = compile_project_metered(ed.project(), &ctx, RATE, 256, Some(&bank)).unwrap();
        let mut renderer = OfflineRenderer::new(compiled.plan, 2);
        renderer.send(Message::Transport(crate::TransportCommand::Play));
        renderer.render(FRAMES, 256);
        let slot = |strip| ow_ipc::meter_slot(ed.project(), strip).unwrap();
        let (peak_a, rms_a) = bank.take_meter(slot(MixerRef::Track(a))).unwrap();
        let (peak_b, rms_b) = bank.take_meter(slot(MixerRef::Track(b))).unwrap();
        let (peak_master, _) = bank.take_meter(slot(MixerRef::Master)).unwrap();
        assert!((peak_a[0] - 0.1414).abs() < 0.01, "{peak_a:?}");
        assert!((rms_a[0] - SOURCE_RMS).abs() < 2e-3, "{rms_a:?}");
        assert!((peak_b[1] - 0.0707).abs() < 0.01, "{peak_b:?}");
        assert!((rms_b[0] - SOURCE_RMS / 2.0).abs() < 2e-3, "{rms_b:?}");
        assert!(peak_master[0] > peak_a[0], "the master carries both tracks");
        // Unmetered compiles leave the bank alone.
        let silent = TelemetryBank::new();
        let _ = compile_project(ed.project(), &ctx, RATE, 256).unwrap();
        assert_eq!(silent.take_meter(0).unwrap().0, [0.0, 0.0]);
    }

    #[test]
    fn compile_rejects_an_invalid_project() {
        let mut project = Project::new("bad");
        project.settings.sample_rate = 0;
        assert!(matches!(
            compile_project(&project, &CompileContext::default(), RATE, 256),
            Err(CompileError::Model(_))
        ));
        assert!(matches!(
            compile_project(&Project::new("ok"), &CompileContext::default(), 0, 256),
            Err(CompileError::Graph(GraphError::InvalidFormat))
        ));
    }
}
