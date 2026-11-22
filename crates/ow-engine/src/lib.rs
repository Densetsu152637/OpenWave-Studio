//! Real-time audio engine for OpenWave Studio.
//!
//! The [`Engine`] handle opens the default output and plays a 440 Hz test tone on request. It
//! runs on the graph core: a [`Graph`] of [`Node`]s is compiled into a [`RenderPlan`] (with
//! delay compensation, see [`pdc`]), sent to the audio thread over a wait-free ring, and run by
//! an [`AudioCore`] each device callback, with a pool of real-time DSP workers running
//! independent nodes in parallel (see [`sched`]). The callback stays allocation-free and
//! lock-free; control flows in through [`ProducerRing`]s carrying [`Message`]s. The transport,
//! tempo map and sample-accurate block timing live in [`transport`].

pub mod graph;
pub mod offline;
pub mod pdc;
pub mod plan;
pub mod rings;
pub mod sched;
pub mod transport;
pub mod workgroup;

use std::f32::consts::TAU;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use ow_io::{OutputInfo, OutputStream};

pub use graph::nodes::{GainNode, MasterNode, OscillatorNode, SumNode, TestToneNode};
pub use graph::{
    AudioCore, ControlLink, CoreBuilder, Graph, GraphError, Inputs, MAX_BLOCK, MAX_WORKERS,
    Message, MidiMessage, Node, NodeId, Outputs, ProcessContext, SwapError, Telemetry,
    default_worker_count,
};
pub use offline::OfflineRenderer;
pub use pdc::DelayNode;
pub use plan::RenderPlan;
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

/// Handle to the running engine. `Send + Sync`; dropping it stops audio.
///
/// The device callback runs an [`AudioCore`] whose graph is the test tone feeding a stereo
/// master, with up to [`default_worker_count`] real-time DSP workers (joined to the output
/// device's audio workgroup on macOS); only workers that obtain real-time priority take part
/// ([`dsp_workers`](Self::dsp_workers)). Later stages replace that plan through the same [`ControlLink`].
pub struct Engine {
    shared: Arc<Shared>,
    control: Mutex<ControlLink>,
    telemetry: Arc<Telemetry>,
    tone_node: NodeId,
    stream: OutputStream,
}

type Started = Result<(ControlLink, NodeId), String>;

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

impl Engine {
    /// Opens the default output device and starts the (silent) engine.
    pub fn start() -> Result<Self, EngineError> {
        let shared = Arc::new(Shared::default());
        let callback_shared = Arc::clone(&shared);
        let (started_tx, started_rx) = mpsc::channel::<Started>();
        let stream = OutputStream::open_default(move |info: &OutputInfo| {
            let mut builder = CoreBuilder::new()
                .workers(default_worker_count())
                .realtime_workers(info.sample_rate, 0);
            if let Ok(workgroup) = AudioWorkgroup::default_output() {
                builder = builder.audio_workgroup(workgroup);
            }
            let (mut link, mut core) = builder.build();
            let started = smoke_plan(info.sample_rate)
                .map_err(|error| error.to_string())
                .and_then(|(plan, tone)| {
                    link.swap_plan(plan).map_err(|error| error.to_string())?;
                    Ok((link, tone))
                });
            let _ = started_tx.send(started);
            let sample_rate = info.sample_rate.max(1) as f32;
            let mut cpu = 0.0_f32;
            move |buf: &mut [f32], channels: usize| {
                let started = Instant::now();
                core.process(buf, channels, None);
                let frames = (buf.len() / channels.max(1)) as f32;
                let budget = frames / sample_rate;
                if budget > 0.0 {
                    let load = started.elapsed().as_secs_f32() / budget;
                    cpu += (load - cpu) * CPU_SMOOTHING;
                    callback_shared.cpu.store(cpu.to_bits(), Ordering::Relaxed);
                }
            }
        })
        .map_err(|error| EngineError(error.to_string()))?;
        let (control, tone_node) = started_rx
            .recv()
            .map_err(|_| EngineError("the audio stream did not report its graph".into()))?
            .map_err(EngineError)?;
        Ok(Self {
            shared,
            telemetry: Arc::clone(control.telemetry()),
            control: Mutex::new(control),
            tone_node,
            stream,
        })
    }

    /// The opened output device and format.
    pub fn output(&self) -> &OutputInfo {
        self.stream.info()
    }

    /// Starts or stops the 440 Hz test tone. Returns whether the request reached the audio
    /// thread; [`test_tone`](Self::test_tone) only changes when it did, so the two cannot
    /// drift. A full control ring is retried briefly before giving up.
    pub fn set_test_tone(&self, on: bool) -> bool {
        let Ok(mut control) = self.control.lock() else {
            return false;
        };
        let mut message = Message::SetParam {
            node: self.tone_node,
            param: graph::nodes::TONE_PARAM_ON,
            value: if on { 1.0 } else { 0.0 },
        };
        for attempt in 0..SEND_ATTEMPTS {
            match control.try_send(message) {
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

    pub fn test_tone(&self) -> bool {
        self.shared.tone.load(Ordering::Relaxed)
    }

    /// DSP workers taking part in the callback's cycles: those that obtained real-time
    /// priority. Workers that could not be promoted exit, and the callback thread covers their
    /// share, so the engine's parallelism is sized by this count (it settles shortly after
    /// [`start`](Self::start), as workers promote themselves).
    pub fn dsp_workers(&self) -> u32 {
        self.telemetry.realtime_workers()
    }

    /// Smoothed audio callback load, 0.0 (idle) to 1.0 (the whole buffer period).
    pub fn cpu_load(&self) -> f32 {
        f32::from_bits(self.shared.cpu.load(Ordering::Relaxed))
    }
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
}
