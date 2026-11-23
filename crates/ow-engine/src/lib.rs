//! Real-time audio engine for OpenWave Studio.
//!
//! The [`Engine`] handle opens the default output and plays a 440 Hz test tone on request. It
//! runs on the graph core: a [`Graph`] of [`Node`]s is compiled into a [`RenderPlan`] (with
//! delay compensation, see [`pdc`]), sent to the audio thread over a wait-free ring, and run by
//! an [`AudioCore`] each device callback, with a pool of real-time DSP workers running
//! independent nodes in parallel (see [`sched`]). The callback stays allocation-free and
//! lock-free; control flows in through [`ProducerRing`]s carrying [`Message`]s. The transport,
//! tempo map and sample-accurate block timing live in [`transport`]. [`compile`] turns an
//! `ow-model` project into a plan and timeline.

pub mod clips;
pub mod compile;
pub mod device;
pub mod export;
pub mod graph;
pub mod midi_hub;
pub mod offline;
pub mod pdc;
pub mod plan;
pub mod record;
pub mod rings;
pub mod sched;
pub mod transport;
pub mod workgroup;

use std::f32::consts::TAU;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

use ow_io::{
    AudioCallback, AudioSelection, AudioStatus, BackendRegistry, IoError, MidiBackend,
    MidirBackend, Opener, RecoveryPolicy, StreamInfo, Supervisor,
};
use ow_ipc::{Overflow, TelemetryBank};
use ow_model::{MediaId, Project, TrackId};

use crate::midi_hub::MidiRings;

pub use clips::{LoadedMedia, SourceCache};
pub use compile::{
    Carry, CompileContext, CompileError, Compiled, NodeDiff, NodeKey, NodeMap, compile_project,
    compile_project_metered, fader_gain,
};
pub use device::{DeviceNode, MAX_EVENTS_PER_BLOCK};
pub use graph::clip::{ClipPlacement, ClipSourceNode};
pub use graph::nodes::{GainNode, MasterNode, OscillatorNode, SumNode, TestToneNode};
pub use graph::{
    AudioCore, ControlLink, CoreBuilder, Graph, GraphError, Inputs, MAX_BLOCK, MAX_WORKERS,
    Message, MidiMessage, Node, NodeId, Outputs, ProcessContext, RecordSwapError, SwapError,
    Telemetry, default_worker_count,
};
pub use midi_hub::{
    MIDI_RING_CAPACITY, MIDI_RINGS, MidiHub, MidiInputSelection, MidiPortState, MidiStatus,
};
pub use offline::OfflineRenderer;
pub use pdc::DelayNode;
pub use plan::RenderPlan;
pub use record::{FinishedTake, RecordError, RecordSet, RecordShared, Recording, Stopped};
pub use rings::{ProducerRing, RingSender, producer_ring};
pub use transport::{
    BlockTiming, Click, MetronomeNode, Span, Spans, Timeline, TimelineSwapError, Transport,
    TransportCommand,
};
pub use workgroup::{AudioWorkgroup, WorkgroupError, WorkgroupMembership};

/// Test tone frequency in hertz.
pub const TEST_TONE_HZ: f32 = 440.0;
/// Test tone peak amplitude (linear), about -14 dBFS so it is safe on loud outputs.
const TEST_TONE_GAIN: f32 = 0.2;
/// Fade time in seconds when the tone starts or stops, to avoid clicks.
const FADE_SECONDS: f32 = 0.01;
/// Tries (1 ms apart) before a control message is reported as not sent.
const SEND_ATTEMPTS: u32 = 5;
/// How long [`Engine::stop_recording`] waits for the audio thread to end the recording.
pub const STOP_WAIT: Duration = Duration::from_secs(2);
/// Weight of the newest block in the smoothed CPU load.
const CPU_SMOOTHING: f32 = 0.1;

/// Why the engine could not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError(String);

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EngineError {}

/// Sine generator with a linear fade, so toggling it never clicks.
#[derive(Debug, Clone)]
pub struct ToneGenerator {
    phase: f32,
    phase_step: f32,
    level: f32,
    level_step: f32,
}

impl ToneGenerator {
    pub fn new(sample_rate: u32, frequency: f32) -> Self {
        let rate = sample_rate.max(1) as f32;
        Self {
            phase: 0.0,
            phase_step: TAU * frequency / rate,
            level: 0.0,
            level_step: TEST_TONE_GAIN / (FADE_SECONDS * rate),
        }
    }

    /// Adds the tone to an interleaved buffer (the same value on every channel of a frame).
    /// `active` selects whether the level fades towards full gain or towards silence.
    pub fn render(&mut self, buf: &mut [f32], channels: usize, active: bool) {
        let channels = channels.max(1);
        for frame in buf.chunks_mut(channels) {
            self.level = if active {
                (self.level + self.level_step).min(TEST_TONE_GAIN)
            } else {
                (self.level - self.level_step).max(0.0)
            };
            let value = self.phase.sin() * self.level;
            self.phase += self.phase_step;
            if self.phase >= TAU {
                self.phase -= TAU;
            }
            for sample in frame {
                *sample += value;
            }
        }
    }
}

#[derive(Debug, Default)]
struct Shared {
    tone: AtomicBool,
    /// Smoothed callback load (0.0 to 1.0 and above when overrunning), as `f32` bits.
    cpu: AtomicU32,
}

/// What the engine needs to talk to the audio core of the currently open stream.
struct Live {
    control: ControlLink,
    telemetry: Arc<Telemetry>,
    /// The smoke-path test tone; `None` once a project plan replaced it.
    tone_node: Option<NodeId>,
    /// The node table of the plan that is live (empty for the smoke plan).
    nodes: NodeMap,
    /// The project the live plan was compiled from.
    project: Option<Arc<Project>>,
    sample_rate: u32,
    /// Input channels the stream delivers (0 when it has no input).
    input_channels: u16,
    /// Round-trip latency of the stream in frames (`StreamInfo::latency_frames`).
    latency_frames: u32,
    /// A timeline that did not fit its ring when its plan was swapped in; retried first at the
    /// next swap, so tempo never stays behind the plan for longer than one edit.
    pending_timeline: Option<Box<Timeline>>,
}

/// The project the engine plays and where its media is: the latest one handed to
/// [`Engine::set_project`].
#[derive(Debug, Clone)]
struct Wanted {
    project: Arc<Project>,
    ctx: CompileContext,
}

type ProjectSlot = Arc<Mutex<Option<Wanted>>>;

/// What [`Engine::set_project`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSwap {
    /// The plan was sent to the audio thread. False when no stream is open: the project is
    /// kept and compiled as soon as one opens.
    pub applied: bool,
    /// The plan is live but its tempo and time-signature maps did not fit the audio thread's
    /// ring yet; they are retried at the next swap.
    pub timeline_pending: bool,
    /// Media the plan's clips need that could not be loaded (see
    /// [`Compiled::missing_media`]); those clips are silent. Empty when `applied` is false.
    pub missing_media: Vec<MediaId>,
    /// Which node keys survived the swap (their state carries over by key), appeared or went
    /// away, compared with the previous live plan. Empty when `applied` is false.
    pub diff: NodeDiff,
}

/// Why [`Engine::set_project`] failed. The previous plan keeps playing.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectError {
    /// The project could not be compiled.
    Compile(CompileError),
    /// The audio thread's plan or timeline queue was full.
    Busy,
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(error) => error.fmt(f),
            Self::Busy => f.write_str("the audio engine is busy; try again"),
        }
    }
}

impl std::error::Error for ProjectError {}

type Started = Result<(Live, MidiRings), String>;

/// How to start the engine.
#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    /// Backend and device choice; the default selects the system default output.
    pub audio: AudioSelection,
    /// Retry behaviour after device loss.
    pub recovery: RecoveryPolicy,
    /// MIDI inputs to connect once audio is running.
    pub midi_inputs: Vec<MidiInputSelection>,
}

/// Compiles the smoke-path graph (test tone to stereo master) for `sample_rate`.
fn smoke_plan(sample_rate: u32) -> Result<(RenderPlan, NodeId), GraphError> {
    let mut graph = Graph::new();
    let tone = graph.add_node(Box::new(TestToneNode::new()));
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(tone, 0, master, 0)?;
    graph.connect(tone, 0, master, 1)?;
    graph.set_master(master)?;
    Ok((graph.compile(sample_rate, MAX_BLOCK)?, tone))
}

/// Handle to the running engine. `Send + Sync`; dropping it stops audio.
///
/// The device callback runs an [`AudioCore`] whose graph is the test tone feeding a stereo
/// master, with up to [`default_worker_count`] real-time DSP workers (joined to the output
/// device's audio workgroup on macOS); only workers that obtain real-time priority take part
/// ([`dsp_workers`](Self::dsp_workers)). Later stages replace that plan through the same [`ControlLink`].
///
/// A [`Supervisor`] keeps the device stream alive: after device loss it reopens the chosen (or
/// the default) device with bounded retries, building a fresh core each time, and
/// [`reconfigure_audio`](Self::reconfigure_audio) switches device, rate or buffer size while
/// running. [`audio_status`](Self::audio_status) reports the state and the last error. MIDI
/// inputs connect through the [`MidiHub`], one ring per input.
pub struct Engine {
    shared: Arc<Shared>,
    live: Arc<Mutex<Option<Live>>>,
    registry: BackendRegistry,
    midi: Arc<MidiHub>,
    supervisor: Supervisor,
    project: ProjectSlot,
    bank: Arc<TelemetryBank>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Engine {
    /// Opens the default output device and starts the (silent) engine, failing when no device
    /// could be opened. The app uses [`start_with`](Self::start_with), which never fails and
    /// recovers in the background.
    pub fn start() -> Result<Self, EngineError> {
        let engine = Self::start_with(EngineConfig::default());
        match engine.audio_status().error {
            Some(error) if engine.audio_status().info.is_none() => Err(EngineError(error)),
            _ => Ok(engine),
        }
    }

    /// Starts the engine on the system's audio backends and MIDI ports. A failed first open
    /// leaves the engine in [`ow_io::AudioState::Recovering`]; see [`audio_status`](Self::audio_status).
    pub fn start_with(config: EngineConfig) -> Self {
        Self::with_backends(
            BackendRegistry::system(),
            Arc::new(MidirBackend::new()),
            config,
        )
    }

    /// Starts the engine on explicit backends (the mock backends in tests).
    pub fn with_backends(
        registry: BackendRegistry,
        midi_backend: Arc<dyn MidiBackend>,
        config: EngineConfig,
    ) -> Self {
        let shared = Arc::new(Shared::default());
        let live: Arc<Mutex<Option<Live>>> = Arc::new(Mutex::new(None));
        let project: ProjectSlot = Arc::new(Mutex::new(None));
        let bank = Arc::new(TelemetryBank::new());
        let midi = Arc::new(MidiHub::new(midi_backend));
        midi.set_inputs(config.midi_inputs);

        let opener: Opener = {
            let shared = Arc::clone(&shared);
            let live = Arc::clone(&live);
            let registry = registry.clone();
            let midi = Arc::clone(&midi);
            let project = Arc::clone(&project);
            let bank = Arc::clone(&bank);
            Box::new(move |selection: &AudioSelection| {
                // The old stream is already closed; its core and rings are gone.
                midi.detach();
                *lock(&live) = None;
                let (started_tx, started_rx) = mpsc::channel::<Started>();
                let callback_shared = Arc::clone(&shared);
                let callback_bank = Arc::clone(&bank);
                let wanted = lock(&project).clone();
                let opened = registry.open(
                    selection,
                    Box::new(move |info: &StreamInfo| {
                        make_callback(info, callback_shared, callback_bank, wanted, started_tx)
                    }),
                );
                let stream = opened?;
                let (new_live, rings) = started_rx
                    .try_recv()
                    .map_err(|_| IoError::new("starting the engine", "no audio core was built"))?
                    .map_err(|error| IoError::new("building the audio graph", error))?;
                shared.tone.store(false, Ordering::Relaxed);
                {
                    let mut guard = lock(&live);
                    let mut new_live = new_live;
                    // `set_project` may have stored a newer project while this stream was
                    // opening (it found no live stream); bring the new plan up to date. The
                    // lock order is always live, then project.
                    let current = lock(&project).clone();
                    let stale = match (&current, &new_live.project) {
                        (Some(a), Some(b)) => !Arc::ptr_eq(&a.project, b),
                        (Some(_), None) => true,
                        (None, _) => false,
                    };
                    if stale && let Some(current) = current {
                        let _ =
                            install_project(&mut new_live, current.project, &current.ctx, &bank);
                    }
                    *guard = Some(new_live);
                }
                midi.attach(rings);
                Ok(stream)
            })
        };
        let supervisor = Supervisor::start(opener, config.audio, config.recovery);
        Self {
            shared,
            live,
            registry,
            midi,
            supervisor,
            project,
            bank,
        }
    }

    /// The telemetry bank the audio thread writes: strip meters, transport position, CPU load
    /// and overflow counters (see `ow-ipc`). It outlives stream reopenings.
    pub fn telemetry_bank(&self) -> &Arc<TelemetryBank> {
        &self.bank
    }

    /// Makes `project` what the engine plays: compiles it for the live stream's sample rate
    /// ([`compile_project_metered`], meters on the telemetry bank; audio clips load their media
    /// through `ctx`, which is kept with the project) and sends the plan and
    /// timeline to the audio thread, which swaps them in at its next cycle boundary. Node state
    /// carries over by [`NodeKey`] ([`NodeMap::diff`], reported in the result); the compiled
    /// nodes hold none yet. With no stream open the project is kept and compiled when one
    /// opens, after a device change or after recovery too.
    ///
    /// Replaces the smoke-path test tone ([`set_test_tone`](Self::set_test_tone) then returns
    /// false), so the app only calls this once a project is being edited. Runs on the caller's
    /// (control) thread and allocates.
    ///
    /// The project is remembered only once its plan is live (or when no stream is open), so a
    /// failed swap leaves the engine and [`current_project`](Self::current_project) on the
    /// previous project, consistently.
    pub fn set_project(
        &self,
        project: Project,
        ctx: CompileContext,
    ) -> Result<ProjectSwap, ProjectError> {
        let project = Arc::new(project);
        let mut guard = lock(&self.live);
        match guard.as_mut() {
            Some(live) => {
                let swap = install_project(live, Arc::clone(&project), &ctx, &self.bank)?;
                *lock(&self.project) = Some(Wanted { project, ctx });
                Ok(swap)
            }
            None => {
                *lock(&self.project) = Some(Wanted { project, ctx });
                Ok(ProjectSwap::default())
            }
        }
    }

    /// The project the engine plays (or will play once a stream opens).
    pub fn current_project(&self) -> Option<Arc<Project>> {
        lock(&self.project)
            .as_ref()
            .map(|wanted| Arc::clone(&wanted.project))
    }

    /// True once a project plan replaced the smoke-path test tone on the live stream.
    pub fn project_plan_live(&self) -> bool {
        lock(&self.live)
            .as_ref()
            .is_some_and(|live| live.project.is_some())
    }

    /// State of the audio device: running, recovering after a failure, or failed, with the
    /// opened device and format and the last error.
    pub fn audio_status(&self) -> AudioStatus {
        self.supervisor.status()
    }

    /// Switches device, backend, sample rate or buffer size. Audio stops briefly while the old
    /// stream closes and the new one opens (the test tone resets). On failure the previous
    /// selection is restored when possible and the error is returned.
    pub fn reconfigure_audio(&self, selection: AudioSelection) -> Result<AudioStatus, IoError> {
        self.supervisor.reconfigure(selection)
    }

    /// Starts a fresh round of reopen attempts after the engine gave up.
    pub fn retry_audio(&self) {
        self.supervisor.retry();
    }

    /// The audio backends this engine can open.
    pub fn backends(&self) -> &BackendRegistry {
        &self.registry
    }

    /// The MIDI input router.
    pub fn midi(&self) -> &Arc<MidiHub> {
        &self.midi
    }

    /// Starts or stops the 440 Hz test tone. Returns whether the request reached the audio
    /// thread; [`test_tone`](Self::test_tone) only changes when it did, so the two cannot
    /// drift. A full control ring is retried briefly before giving up.
    pub fn set_test_tone(&self, on: bool) -> bool {
        let mut guard = lock(&self.live);
        let Some(live) = guard.as_mut() else {
            return false;
        };
        let Some(tone_node) = live.tone_node else {
            return false;
        };
        let mut message = Message::SetParam {
            node: tone_node,
            param: graph::nodes::TONE_PARAM_ON,
            value: if on { 1.0 } else { 0.0 },
        };
        for attempt in 0..SEND_ATTEMPTS {
            match live.control.try_send(message) {
                Ok(()) => {
                    self.shared.tone.store(on, Ordering::Relaxed);
                    return true;
                }
                Err(returned) => {
                    message = returned;
                    if attempt + 1 < SEND_ATTEMPTS {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        }
        false
    }

    /// Sends a transport command (play, stop, locate) to the audio thread. Returns whether it
    /// reached the thread; false when no stream is open or the control ring stayed full.
    pub fn send_transport(&self, command: TransportCommand) -> bool {
        let mut guard = lock(&self.live);
        let Some(live) = guard.as_mut() else {
            return false;
        };
        send_message(live, Message::Transport(command))
    }

    /// Starts recording one take per `(track, wav path)`: creates the files, hands the record
    /// tap to the audio thread, and arms and starts the transport from where it is. The takes
    /// are 32-bit float WAVs at the stream's sample rate, mono when the input has one channel
    /// and stereo (input channels 0 and 1) otherwise. Fails when no stream or no input is open,
    /// when `takes` is empty, or when a file cannot be created. See [`record`] for how takes are
    /// placed. Runs on the control thread; creates files and threads.
    pub fn start_recording(&self, takes: &[(TrackId, PathBuf)]) -> Result<Recording, RecordError> {
        let mut guard = lock(&self.live);
        let live = guard.as_mut().ok_or(RecordError::NoStream)?;
        if live.input_channels == 0 {
            return Err(RecordError::NoInput);
        }
        let (recording, set) = Recording::begin(
            takes,
            live.sample_rate,
            live.input_channels.min(2),
            live.latency_frames,
        )?;
        let mut pending = set;
        for attempt in 0..SEND_ATTEMPTS {
            match live.control.swap_record(pending) {
                Ok(()) => break,
                Err(RecordSwapError(returned)) => {
                    if attempt + 1 == SEND_ATTEMPTS {
                        return Err(RecordError::Busy);
                    }
                    pending = *returned;
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        let started = send_message(
            live,
            Message::Transport(TransportCommand::SetRecordArm(true)),
        ) && send_message(live, Message::Transport(TransportCommand::Play));
        if !started {
            // Take the tap back out (best effort); the writers finish when `recording` drops.
            send_message(live, Message::EndRecording);
            send_message(
                live,
                Message::Transport(TransportCommand::SetRecordArm(false)),
            );
            return Err(RecordError::Busy);
        }
        Ok(recording)
    }

    /// Stops the transport, ends `recording` and finishes its take files. The audio thread
    /// confirms it will push no more frames before the files are finished, so each take holds
    /// every frame that was captured ([`RecordShared::captured`]). Waits up to
    /// [`STOP_WAIT`] for that confirmation (an audio stream that was closed meanwhile counts as
    /// confirmed); [`Stopped::complete`] is false when it did not arrive.
    pub fn stop_recording(&self, recording: Recording) -> Result<Stopped, RecordError> {
        {
            let mut guard = lock(&self.live);
            if let Some(live) = guard.as_mut() {
                send_message(live, Message::Transport(TransportCommand::Stop));
                send_message(
                    live,
                    Message::Transport(TransportCommand::SetRecordArm(false)),
                );
                send_message(live, Message::EndRecording);
            }
        }
        let deadline = Instant::now() + STOP_WAIT;
        let complete = loop {
            // Only the tap holds the other reference: gone means the stream was closed.
            if recording.shared.done() || Arc::strong_count(&recording.shared) == 1 {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        if let Some(live) = lock(&self.live).as_mut() {
            live.control.collect_retired();
        }
        recording.finish().map(|takes| Stopped { takes, complete })
    }

    pub fn test_tone(&self) -> bool {
        self.shared.tone.load(Ordering::Relaxed)
    }

    /// DSP workers taking part in the callback's cycles: those that obtained real-time
    /// priority. Workers that could not be promoted exit, and the callback thread covers their
    /// share, so the engine's parallelism is sized by this count (it settles shortly after
    /// the stream opens, as workers promote themselves).
    pub fn dsp_workers(&self) -> u32 {
        lock(&self.live)
            .as_ref()
            .map_or(0, |live| live.telemetry.realtime_workers())
    }

    /// Smoothed audio callback load, 0.0 (idle) to 1.0 (the whole buffer period).
    pub fn cpu_load(&self) -> f32 {
        f32::from_bits(self.shared.cpu.load(Ordering::Relaxed))
    }
}

/// Sends one control message to the audio thread, retrying briefly while its ring is full.
fn send_message(live: &mut Live, mut message: Message) -> bool {
    for attempt in 0..SEND_ATTEMPTS {
        match live.control.try_send(message) {
            Ok(()) => return true,
            Err(returned) => {
                message = returned;
                if attempt + 1 < SEND_ATTEMPTS {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
    false
}

/// Compiles `project` for the live stream and sends plan and timeline to the audio thread.
/// Nothing changes when compiling fails or the plan queue stays full; once the plan is queued
/// the live bookkeeping (nodes, project) is updated with it, and a timeline that does not fit
/// is kept for the next swap.
fn install_project(
    live: &mut Live,
    project: Arc<Project>,
    ctx: &CompileContext,
    bank: &Arc<TelemetryBank>,
) -> Result<ProjectSwap, ProjectError> {
    if let Some(pending) = live.pending_timeline.take() {
        live.pending_timeline = send_timeline(&mut live.control, pending);
    }
    let compiled = compile_project_metered(&project, ctx, live.sample_rate, MAX_BLOCK, Some(bank))
        .map_err(ProjectError::Compile)?;
    let Compiled {
        mut plan,
        timeline,
        nodes,
        missing_media,
    } = compiled;
    for attempt in 0..SEND_ATTEMPTS {
        match live.control.swap_plan(plan) {
            Ok(()) => break,
            Err(SwapError(returned)) => {
                if attempt + 1 == SEND_ATTEMPTS {
                    return Err(ProjectError::Busy);
                }
                plan = *returned;
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    let diff = live.nodes.diff(&nodes);
    live.nodes = nodes;
    live.tone_node = None;
    live.project = Some(project);
    live.pending_timeline = send_timeline(&mut live.control, Box::new(timeline));
    Ok(ProjectSwap {
        applied: true,
        timeline_pending: live.pending_timeline.is_some(),
        missing_media,
        diff,
    })
}

/// Sends a timeline with a few short retries; hands it back when the ring stays full.
fn send_timeline(control: &mut ControlLink, mut timeline: Box<Timeline>) -> Option<Box<Timeline>> {
    for attempt in 0..SEND_ATTEMPTS {
        match control.swap_timeline(*timeline) {
            Ok(()) => return None,
            Err(TimelineSwapError(returned)) => {
                timeline = returned;
                if attempt + 1 < SEND_ATTEMPTS {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
    Some(timeline)
}

/// The first plan of a stream: the project's when one is set, else the smoke plan. Returns the
/// test-tone node (smoke plan only), the node table and the node MIDI is routed to.
fn first_plan(
    link: &mut ControlLink,
    wanted: Option<&Wanted>,
    sample_rate: u32,
    bank: &Arc<TelemetryBank>,
) -> Result<(Option<NodeId>, NodeMap, NodeId), String> {
    match wanted {
        Some(Wanted { project, ctx }) => {
            let compiled =
                compile_project_metered(project, ctx, sample_rate, MAX_BLOCK, Some(bank))
                    .map_err(|error| error.to_string())?;
            let target = compiled
                .nodes
                .fader(ow_model::MixerRef::Master)
                .ok_or_else(|| "the compiled project has no master".to_string())?;
            link.swap_timeline(compiled.timeline)
                .map_err(|error| error.to_string())?;
            link.swap_plan(compiled.plan)
                .map_err(|error| error.to_string())?;
            Ok((None, compiled.nodes, target))
        }
        None => {
            let (plan, tone) = smoke_plan(sample_rate).map_err(|error| error.to_string())?;
            link.swap_plan(plan).map_err(|error| error.to_string())?;
            Ok((Some(tone), NodeMap::default(), tone))
        }
    }
}

/// Runs on the stream thread once the device format is known: builds the core, its MIDI rings
/// and the first plan, hands the control side to the opener and returns the render callback.
fn make_callback(
    info: &StreamInfo,
    shared: Arc<Shared>,
    bank: Arc<TelemetryBank>,
    wanted: Option<Wanted>,
    started_tx: mpsc::Sender<Started>,
) -> AudioCallback {
    let mut builder = CoreBuilder::new()
        .workers(default_worker_count())
        .realtime_workers(info.sample_rate, info.buffer_frames.unwrap_or(0));
    if let Ok(workgroup) = AudioWorkgroup::default_output() {
        builder = builder.audio_workgroup(workgroup);
    }
    let pool: Vec<RingSender<Message>> = (0..MIDI_RINGS)
        .map(|_| builder.producer_ring(MIDI_RING_CAPACITY))
        .collect();
    let control_surface = builder.producer_ring(MIDI_RING_CAPACITY);
    let (mut link, mut core) = builder.build();
    let telemetry = Arc::clone(link.telemetry());
    let started = first_plan(&mut link, wanted.as_ref(), info.sample_rate, &bank).map(
        |(tone_node, nodes, target)| {
            (
                Live {
                    telemetry: Arc::clone(&telemetry),
                    control: link,
                    tone_node,
                    nodes,
                    project: wanted.as_ref().map(|w| Arc::clone(&w.project)),
                    sample_rate: info.sample_rate,
                    input_channels: info.input_channels,
                    latency_frames: info.latency_frames,
                    pending_timeline: None,
                },
                MidiRings {
                    pool,
                    control_surface,
                    // Until the MIDI stages route to instruments, MIDI goes to a node that
                    // ignores it (the tone, or the master fader); the ring path and counters
                    // are exercised end to end.
                    target,
                },
            )
        },
    );
    let _ = started_tx.send(started);
    let sample_rate = info.sample_rate.max(1) as f32;
    let channels = usize::from(info.output_channels).max(1);
    let mut cpu = 0.0_f32;
    Box::new(move |buf: &mut [f32], input: Option<&[f32]>| {
        let started = Instant::now();
        core.process(buf, channels, input);
        let frames = (buf.len() / channels) as f32;
        let budget = frames / sample_rate;
        if budget > 0.0 {
            let load = started.elapsed().as_secs_f32() / budget;
            cpu += (load - cpu) * CPU_SMOOTHING;
            shared.cpu.store(cpu.to_bits(), Ordering::Relaxed);
        }
        // Relaxed atomic stores into the preallocated bank: nothing here allocates or locks.
        bank.set_position(telemetry.transport_position());
        bank.set_cpu_load(cpu);
        bank.set_overflow(Overflow::Ring, core.overflow_total());
        bank.set_overflow(Overflow::Unrouted, telemetry.unrouted());
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_block(
        tone: &mut ToneGenerator,
        frames: usize,
        channels: usize,
        active: bool,
    ) -> Vec<f32> {
        let mut buf = vec![0.0; frames * channels];
        tone.render(&mut buf, channels, active);
        buf
    }

    #[test]
    fn inactive_tone_is_silent() {
        let mut tone = ToneGenerator::new(48_000, TEST_TONE_HZ);
        assert!(
            render_block(&mut tone, 4_800, 2, false)
                .iter()
                .all(|s| *s == 0.0)
        );
    }

    #[test]
    fn active_tone_has_the_requested_frequency() {
        let rate = 48_000;
        let mut tone = ToneGenerator::new(rate, TEST_TONE_HZ);
        // Skip the fade-in, then count upward zero crossings over exactly one second.
        render_block(&mut tone, 4_800, 1, true);
        let buf = render_block(&mut tone, rate as usize, 1, true);
        let crossings = buf.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((439..=441).contains(&crossings), "crossings = {crossings}");
        let peak = buf.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((peak - TEST_TONE_GAIN).abs() < 0.01, "peak = {peak}");
    }

    #[test]
    fn tone_is_copied_to_every_channel_and_added_to_existing_audio() {
        let mut tone = ToneGenerator::new(48_000, TEST_TONE_HZ);
        render_block(&mut tone, 4_800, 2, true);
        let mut buf = vec![0.5; 2 * 100];
        tone.render(&mut buf, 2, true);
        assert!(buf.chunks(2).all(|f| f[0] == f[1]));
        assert!(buf.iter().any(|s| *s != 0.5));
    }

    #[test]
    fn tone_fades_out_without_a_jump() {
        let mut tone = ToneGenerator::new(48_000, TEST_TONE_HZ);
        render_block(&mut tone, 4_800, 1, true);
        let buf = render_block(&mut tone, 4_800, 1, false);
        let max_step = buf
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_step < 0.05, "max step = {max_step}");
        assert!(buf[buf.len() - 1].abs() < 1e-6);
    }

    #[test]
    fn smoke_graph_plays_the_tone_on_both_channels_when_switched_on() {
        let (plan, tone) = smoke_plan(48_000).unwrap();
        let mut renderer = OfflineRenderer::new(plan, 2);
        assert!(renderer.render(2_000, 256).iter().all(|s| *s == 0.0));
        renderer.send(Message::SetParam {
            node: tone,
            param: graph::nodes::TONE_PARAM_ON,
            value: 1.0,
        });
        let out = renderer.render(4_800, 256);
        assert!(out.chunks(2).all(|f| f[0] == f[1]));
        let peak = out.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((peak - TEST_TONE_GAIN).abs() < 0.01, "peak = {peak}");
    }

    fn project_at(bpm: f64) -> Arc<Project> {
        let mut project = Project::new("tempo");
        project.tempo_map = ow_model::time::TempoMap::constant(bpm).unwrap();
        Arc::new(project)
    }

    fn test_live() -> (Live, AudioCore) {
        let (link, core) = CoreBuilder::new()
            .workers(0)
            .plan_queue(64)
            .retire_queue(64)
            .build();
        let live = Live {
            telemetry: Arc::clone(link.telemetry()),
            control: link,
            tone_node: None,
            nodes: NodeMap::default(),
            project: None,
            sample_rate: 48_000,
            input_channels: 0,
            latency_frames: 0,
            pending_timeline: None,
        };
        (live, core)
    }

    /// The sample position 4 beats lands on under the timeline the audio thread has live, which
    /// reveals its tempo: `4 * 60 / bpm * 48000`.
    fn live_tempo(live: &mut Live, core: &mut AudioCore) -> f64 {
        assert!(
            live.control
                .send(Message::Transport(TransportCommand::LocateBeats(4.0)))
        );
        let mut buffer = vec![0.0_f32; 64 * 2];
        for _ in 0..8 {
            core.process(&mut buffer, 2, None);
        }
        4.0 * 60.0 * 48_000.0 / core.transport().position as f64
    }

    #[test]
    fn pending_timeline_never_lets_an_older_tempo_overtake_a_newer_one() {
        let bank = Arc::new(TelemetryBank::new());
        let ctx = CompileContext::default();
        let (mut live, mut core) = test_live();
        let mut installed = 0_u64;
        let mut bpm = 100.0;
        // Fill the timeline ring (the audio thread is not running) until a swap leaves its
        // timeline pending; the plan is live nonetheless.
        let mut last = project_at(bpm);
        while live.pending_timeline.is_none() {
            assert!(installed < 12, "the timeline ring never filled");
            bpm += 1.0;
            last = project_at(bpm);
            let swap = install_project(&mut live, Arc::clone(&last), &ctx, &bank).unwrap();
            installed += 1;
            assert!(swap.applied);
            assert_eq!(swap.timeline_pending, live.pending_timeline.is_some());
        }
        // Plan and bookkeeping are consistent with the swap that left the timeline pending.
        assert!(Arc::ptr_eq(live.project.as_ref().unwrap(), &last));
        assert_eq!(
            live.pending_timeline.as_ref().unwrap().tempo(),
            &last.tempo_map
        );

        // The ring is still full: a newer swap cannot send anything and its timeline replaces
        // the pending one, so the older tempo is dropped instead of queued behind it.
        let newer = project_at(150.0);
        let swap = install_project(&mut live, Arc::clone(&newer), &ctx, &bank).unwrap();
        installed += 1;
        assert!(swap.timeline_pending);
        assert_eq!(
            live.pending_timeline.as_ref().unwrap().tempo(),
            &newer.tempo_map
        );

        // The audio thread catches up; the next swap flushes the pending timeline first and the
        // newest after it.
        let mut buffer = vec![0.0_f32; 64 * 2];
        for _ in 0..4 {
            core.process(&mut buffer, 2, None);
        }
        let newest = project_at(160.0);
        let swap = install_project(&mut live, Arc::clone(&newest), &ctx, &bank).unwrap();
        installed += 1;
        assert!(!swap.timeline_pending && live.pending_timeline.is_none());
        assert!(Arc::ptr_eq(live.project.as_ref().unwrap(), &newest));
        let tempo = live_tempo(&mut live, &mut core);
        assert!((tempo - 160.0).abs() < 0.01, "the live tempo is {tempo}");
        // Every plan reached the audio thread, in step with the project the engine remembers.
        assert_eq!(live.telemetry.plan_swaps(), installed);
    }
}
