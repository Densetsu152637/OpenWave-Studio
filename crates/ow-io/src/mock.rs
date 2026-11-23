//! The mock backend: the realtime path for tests on machines without audio hardware.
//!
//! [`MockBackend`] implements [`AudioBackend`] and drives the engine callback from a plain thread
//! ([`MockDrive::Thread`]) or synchronously from the test ([`MockDrive::Manual`], through
//! [`MockHandle::run_callbacks`]) with a fixed buffer size. It can inject input audio, loop the
//! output back to the input after a set latency, capture everything the callback renders, and
//! simulate device loss. [`MockMidiBackend`] is its MIDI counterpart: each connected input gets
//! its own delivery thread (like a platform MIDI callback thread), fed with timestamped messages
//! from any number of test threads.
//!
//! The harness itself never allocates per callback (buffers are sized at open), so a test that
//! installs `assert_no_alloc`'s `AllocDisabler` sees only what the engine does.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::backend::{
    AudioBackend, AudioCallback, AudioStream, CallbackFactory, DeviceInfo, IoError, IoErrorKind,
    StreamHealth, StreamInfo, StreamRequest, StreamState,
};
use crate::midi::{
    MidiBackend, MidiEvent, MidiHandler, MidiInputConnection, MidiOutputConnection, MidiPortInfo,
};

/// Id of the mock backend.
pub const MOCK_BACKEND_ID: &str = "mock";
/// Id of the single mock device.
pub const MOCK_DEVICE_ID: &str = "mock-device";

/// How the callback is driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockDrive {
    /// Only [`MockHandle::run_callbacks`] runs the callback, on the caller's thread.
    Manual,
    /// A thread runs the callback in a loop while the stream is started. `paced` sleeps one
    /// buffer period between callbacks (real time); unpaced runs as fast as possible.
    Thread { paced: bool },
}

/// Mock device settings.
#[derive(Debug, Clone)]
pub struct MockConfig {
    pub sample_rate: u32,
    pub output_channels: u16,
    /// Input channels; 0 means no input.
    pub input_channels: u16,
    /// Fixed callback size in frames.
    pub buffer_frames: u32,
    pub drive: MockDrive,
    /// Feed the output back to the input after this many frames (at least one buffer, since a
    /// block's input is produced before its output). Requires `input_channels > 0`.
    pub loopback_latency_frames: Option<u32>,
    /// Frames of output to keep for [`MockHandle::captured`]; 0 disables capture.
    pub capture_frames: usize,
    pub device_name: String,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            output_channels: 2,
            input_channels: 0,
            buffer_frames: 64,
            drive: MockDrive::Manual,
            loopback_latency_frames: None,
            capture_frames: 0,
            device_name: "Mock device".into(),
        }
    }
}

/// The mock audio backend. Clone [`MockBackend::handle`] to drive and inspect it.
#[derive(Clone)]
pub struct MockBackend {
    shared: Arc<Shared>,
}

struct Shared {
    config: MockConfig,
    /// Runner of the open stream when it is driven manually.
    manual: Mutex<Option<Runner>>,
    /// State of the currently open stream, for failure injection.
    state: Mutex<Option<Arc<StreamState>>>,
    input: Mutex<VecDeque<f32>>,
    captured: Mutex<Vec<f32>>,
    callbacks: AtomicU64,
    opens: AtomicU64,
    fail_next_opens: AtomicU64,
}

impl MockBackend {
    pub fn new(config: MockConfig) -> Self {
        Self {
            shared: Arc::new(Shared {
                config,
                manual: Mutex::new(None),
                state: Mutex::new(None),
                input: Mutex::new(VecDeque::new()),
                captured: Mutex::new(Vec::new()),
                callbacks: AtomicU64::new(0),
                opens: AtomicU64::new(0),
                fail_next_opens: AtomicU64::new(0),
            }),
        }
    }

    /// Control and inspection handle (cheap to clone, usable from any thread).
    pub fn handle(&self) -> MockHandle {
        MockHandle {
            shared: Arc::clone(&self.shared),
        }
    }
}

/// Controls the mock backend from a test.
#[derive(Clone)]
pub struct MockHandle {
    shared: Arc<Shared>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl MockHandle {
    /// Queues interleaved input audio for the input channels; consumed one block per callback
    /// (silence when the queue is empty). Ignored while a loopback is configured.
    pub fn inject_input(&self, samples: &[f32]) {
        lock(&self.shared.input).extend(samples.iter().copied());
    }

    /// Runs `count` callbacks on the calling thread (manual drive only).
    pub fn run_callbacks(&self, count: usize) -> Result<(), IoError> {
        let mut guard = lock(&self.shared.manual);
        let runner = guard
            .as_mut()
            .ok_or_else(|| IoError::new("mock backend", "no stream is open in manual mode"))?;
        for _ in 0..count {
            runner.run_one(&self.shared);
        }
        Ok(())
    }

    /// Output rendered so far (interleaved), up to `capture_frames` frames.
    pub fn captured(&self) -> Vec<f32> {
        lock(&self.shared.captured).clone()
    }

    /// Callbacks run since the backend was created.
    pub fn callbacks_run(&self) -> u64 {
        self.shared.callbacks.load(Ordering::Relaxed)
    }

    /// Streams opened since the backend was created.
    pub fn opens(&self) -> u64 {
        self.shared.opens.load(Ordering::Relaxed)
    }

    /// Makes the next `count` calls to `open` fail (to exercise recovery retries).
    pub fn fail_next_opens(&self, count: u64) {
        self.shared.fail_next_opens.store(count, Ordering::Relaxed);
    }

    /// Simulates the device disappearing: the open stream turns `Failed`.
    pub fn disconnect(&self) {
        if let Some(state) = lock(&self.shared.state).as_ref() {
            state.fail(true, "mock device disconnected");
        }
    }
}

/// Runs one callback with fixed buffers.
struct Runner {
    callback: AudioCallback,
    out: Vec<f32>,
    input: Vec<f32>,
    out_channels: usize,
    in_channels: usize,
    frames: usize,
    loopback: Option<Loopback>,
    capture_limit: usize,
}

struct Loopback {
    /// One frame per slot; `pos` is both where the next block is read and then overwritten.
    line: Vec<f32>,
    frames: usize,
    pos: usize,
}

impl Runner {
    fn run_one(&mut self, shared: &Shared) {
        let in_channels = self.in_channels;
        if in_channels > 0 {
            match self.loopback.as_mut() {
                Some(lb) => {
                    for frame in 0..self.frames {
                        let slot = ((lb.pos + frame) % lb.frames) * in_channels;
                        self.input[frame * in_channels..(frame + 1) * in_channels]
                            .copy_from_slice(&lb.line[slot..slot + in_channels]);
                    }
                }
                None => {
                    let mut queue = lock(&shared.input);
                    for slot in &mut self.input {
                        *slot = queue.pop_front().unwrap_or(0.0);
                    }
                }
            }
        }
        self.out.fill(0.0);
        let input = (in_channels > 0).then_some(self.input.as_slice());
        (self.callback)(&mut self.out, input);
        if let Some(lb) = self.loopback.as_mut() {
            for frame in 0..self.frames {
                let slot = ((lb.pos + frame) % lb.frames) * in_channels;
                for channel in 0..in_channels {
                    // Output channel c feeds input channel c; extra inputs hear silence.
                    lb.line[slot + channel] = if channel < self.out_channels {
                        self.out[frame * self.out_channels + channel]
                    } else {
                        0.0
                    };
                }
            }
            lb.pos = (lb.pos + self.frames) % lb.frames;
        }
        if self.capture_limit > 0 {
            let mut captured = lock(&shared.captured);
            let room = (self.capture_limit * self.out_channels).saturating_sub(captured.len());
            let take = room.min(self.out.len());
            if take > 0 {
                // Capacity was reserved at open, so this does not allocate.
                captured.extend_from_slice(&self.out[..take]);
            }
        }
        shared.callbacks.fetch_add(1, Ordering::Relaxed);
    }
}

impl AudioBackend for MockBackend {
    fn id(&self) -> String {
        MOCK_BACKEND_ID.into()
    }

    fn label(&self) -> String {
        "Mock (testing)".into()
    }

    fn devices(&self) -> Result<Vec<DeviceInfo>, IoError> {
        let config = &self.shared.config;
        Ok(vec![DeviceInfo {
            id: MOCK_DEVICE_ID.into(),
            name: config.device_name.clone(),
            is_default_output: true,
            is_default_input: config.input_channels > 0,
            max_output_channels: config.output_channels,
            max_input_channels: config.input_channels,
            sample_rates: vec![config.sample_rate],
            buffer_frames: Some((config.buffer_frames, config.buffer_frames)),
        }])
    }

    fn open(
        &self,
        request: &StreamRequest,
        factory: CallbackFactory,
    ) -> Result<Box<dyn AudioStream>, IoError> {
        let shared = &self.shared;
        if shared
            .fail_next_opens
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(IoError::with_kind(
                IoErrorKind::DeviceLost,
                "opening the mock device",
                "injected open failure",
            ));
        }
        if let Some(wanted) = request.output_device.as_deref()
            && wanted != MOCK_DEVICE_ID
            && wanted != crate::backend::DEFAULT_DEVICE
        {
            return Err(IoError::with_kind(
                IoErrorKind::NotFound,
                "opening the mock device",
                format_args!("no device \"{wanted}\""),
            ));
        }
        let config = &shared.config;
        let in_channels = usize::from(config.input_channels);
        let out_channels = usize::from(config.output_channels);
        let frames = config.buffer_frames.max(1) as usize;
        // The loopback line is the whole round trip: a frame rendered now is heard this many
        // frames later (never less than one buffer).
        let latency_frames = match (config.loopback_latency_frames, in_channels) {
            (Some(latency), channels) if channels > 0 => latency.max(config.buffer_frames.max(1)),
            _ => 0,
        };
        let info = StreamInfo {
            backend: MOCK_BACKEND_ID.into(),
            device_id: MOCK_DEVICE_ID.into(),
            device_name: config.device_name.clone(),
            sample_rate: config.sample_rate,
            output_channels: config.output_channels,
            input_channels: config.input_channels,
            buffer_frames: Some(config.buffer_frames),
            exclusive: false,
            latency_frames,
        };
        let callback = factory(&info);
        let loopback = match (config.loopback_latency_frames, in_channels) {
            (Some(latency), channels) if channels > 0 => {
                let latency = (latency as usize).max(frames);
                Some(Loopback {
                    line: vec![0.0; latency * channels],
                    frames: latency,
                    pos: 0,
                })
            }
            _ => None,
        };
        let runner = Runner {
            callback,
            out: vec![0.0; frames * out_channels],
            input: vec![0.0; frames * in_channels],
            out_channels,
            in_channels,
            frames,
            loopback,
            capture_limit: config.capture_frames,
        };
        {
            let mut captured = lock(&shared.captured);
            captured.clear();
            captured.reserve(config.capture_frames * out_channels);
        }
        let state = Arc::new(StreamState::default());
        *lock(&shared.state) = Some(Arc::clone(&state));
        shared.opens.fetch_add(1, Ordering::Relaxed);
        let paced_period =
            Duration::from_secs_f64(frames as f64 / f64::from(config.sample_rate.max(1)));
        let driver = match config.drive {
            MockDrive::Manual => {
                *lock(&shared.manual) = Some(runner);
                Driver::Manual
            }
            MockDrive::Thread { paced } => {
                let (tx, rx) = mpsc::channel::<bool>();
                let thread_shared = Arc::clone(&self.shared);
                let thread_state = Arc::clone(&state);
                let thread = thread::Builder::new()
                    .name("ow-mock-audio".into())
                    .spawn(move || {
                        drive_thread(
                            runner,
                            &thread_shared,
                            &thread_state,
                            &rx,
                            paced,
                            paced_period,
                        );
                    })
                    .map_err(|e| IoError::new("spawning the mock audio thread", e))?;
                Driver::Thread {
                    tx,
                    thread: Some(thread),
                }
            }
        };
        Ok(Box::new(MockStream {
            info,
            state,
            driver,
            shared: Arc::clone(&self.shared),
        }))
    }
}

fn drive_thread(
    mut runner: Runner,
    shared: &Shared,
    state: &StreamState,
    commands: &mpsc::Receiver<bool>,
    paced: bool,
    period: Duration,
) {
    let mut running = false;
    let mut next = Instant::now();
    loop {
        if running {
            match commands.try_recv() {
                Ok(run) => {
                    running = run;
                    next = Instant::now();
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        } else {
            match commands.recv() {
                Ok(run) => {
                    running = run;
                    next = Instant::now();
                }
                Err(_) => return,
            }
        }
        if !running {
            continue;
        }
        if matches!(state.health(), StreamHealth::Failed { .. }) {
            // A failed device delivers no more callbacks; wait to be dropped.
            while commands.recv().is_ok() {}
            return;
        }
        runner.run_one(shared);
        if paced {
            next += period;
            if let Some(wait) = next.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
        }
    }
}

enum Driver {
    Manual,
    Thread {
        tx: Sender<bool>,
        thread: Option<JoinHandle<()>>,
    },
}

/// A stream on the mock device.
pub struct MockStream {
    info: StreamInfo,
    state: Arc<StreamState>,
    driver: Driver,
    shared: Arc<Shared>,
}

impl AudioStream for MockStream {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn start(&mut self) -> Result<(), IoError> {
        if let Driver::Thread { tx, .. } = &self.driver {
            tx.send(true)
                .map_err(|_| IoError::new("mock stream", "driver thread exited"))?;
        }
        self.state.set_running(true);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), IoError> {
        if let Driver::Thread { tx, .. } = &self.driver {
            tx.send(false)
                .map_err(|_| IoError::new("mock stream", "driver thread exited"))?;
        }
        self.state.set_running(false);
        Ok(())
    }

    fn health(&self) -> StreamHealth {
        self.state.health()
    }
}

impl Drop for MockStream {
    fn drop(&mut self) {
        match &mut self.driver {
            Driver::Manual => {
                *lock(&self.shared.manual) = None;
            }
            Driver::Thread { tx, thread } => {
                // Closing the command channel ends the driver thread.
                let (closed, _) = mpsc::channel();
                drop(std::mem::replace(tx, closed));
                if let Some(thread) = thread.take() {
                    let _ = thread.join();
                }
            }
        }
        *lock(&self.shared.state) = None;
    }
}

// ---------------------------------------------------------------------------------------------
// MIDI
// ---------------------------------------------------------------------------------------------

type Connections = Arc<Mutex<Vec<(String, Sender<MidiMsg>)>>>;

enum MidiMsg {
    Event(MidiEvent),
    Flush(Sender<()>),
}

/// Mock MIDI backend with a fixed set of input ports.
///
/// `connect_input` starts a delivery thread per connection that runs the handler, as a platform
/// MIDI layer would. [`MockMidiBackend::send`] injects a timestamped message into a connected
/// port from any thread; [`MockMidiBackend::flush`] waits until everything sent so far has been
/// handled.
#[derive(Clone)]
pub struct MockMidiBackend {
    ports: Arc<Vec<String>>,
    connections: Connections,
}

impl MockMidiBackend {
    pub fn new<S: Into<String>>(ports: impl IntoIterator<Item = S>) -> Self {
        Self {
            ports: Arc::new(ports.into_iter().map(Into::into).collect()),
            connections: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn sender_for(&self, port_id: &str) -> Option<Sender<MidiMsg>> {
        lock(&self.connections)
            .iter()
            .find(|(id, _)| id == port_id)
            .map(|(_, tx)| tx.clone())
    }

    /// Sends a message into a connected input. Returns false when the port is not connected,
    /// the message is empty or longer than three bytes, or the connection closed.
    pub fn send(&self, port_id: &str, timestamp_us: u64, bytes: &[u8]) -> bool {
        let Some(tx) = self.sender_for(port_id) else {
            return false;
        };
        let source = self.ports.iter().position(|p| p == port_id).unwrap_or(0) as u16;
        match MidiEvent::from_bytes(source, timestamp_us, bytes) {
            Some(event) => tx.send(MidiMsg::Event(event)).is_ok(),
            None => false,
        }
    }

    /// Blocks until the port's handler has processed every message sent before this call.
    pub fn flush(&self, port_id: &str) {
        if let Some(tx) = self.sender_for(port_id) {
            let (done_tx, done_rx) = mpsc::channel();
            if tx.send(MidiMsg::Flush(done_tx)).is_ok() {
                let _ = done_rx.recv();
            }
        }
    }
}

struct MockMidiInput {
    port: String,
    connections: Connections,
    tx: Option<Sender<MidiMsg>>,
    thread: Option<JoinHandle<()>>,
}

impl MidiInputConnection for MockMidiInput {}

impl Drop for MockMidiInput {
    fn drop(&mut self) {
        lock(&self.connections).retain(|(id, _)| id != &self.port);
        self.tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct MockMidiOutput;

impl MidiOutputConnection for MockMidiOutput {
    fn send(&mut self, _message: &[u8]) -> Result<(), IoError> {
        Ok(())
    }
}

impl MidiBackend for MockMidiBackend {
    fn inputs(&self) -> Result<Vec<MidiPortInfo>, IoError> {
        Ok(self
            .ports
            .iter()
            .map(|name| MidiPortInfo {
                id: name.clone(),
                name: name.clone(),
            })
            .collect())
    }

    fn outputs(&self) -> Result<Vec<MidiPortInfo>, IoError> {
        Ok(Vec::new())
    }

    fn connect_input(
        &self,
        port_id: &str,
        source: u16,
        mut handler: MidiHandler,
    ) -> Result<Box<dyn MidiInputConnection>, IoError> {
        if !self.ports.iter().any(|p| p == port_id) {
            return Err(IoError::with_kind(
                IoErrorKind::NotFound,
                "connecting MIDI input",
                format_args!("port \"{port_id}\" not found"),
            ));
        }
        let (tx, rx) = mpsc::channel::<MidiMsg>();
        let thread = thread::Builder::new()
            .name("ow-mock-midi".into())
            .spawn(move || {
                while let Ok(message) = rx.recv() {
                    match message {
                        MidiMsg::Event(mut event) => {
                            event.source = source;
                            handler(event);
                        }
                        MidiMsg::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })
            .map_err(|e| IoError::new("spawning the mock MIDI thread", e))?;
        lock(&self.connections).push((port_id.to_string(), tx.clone()));
        Ok(Box::new(MockMidiInput {
            port: port_id.to_string(),
            connections: Arc::clone(&self.connections),
            tx: Some(tx),
            thread: Some(thread),
        }))
    }

    fn connect_output(&self, _port_id: &str) -> Result<Box<dyn MidiOutputConnection>, IoError> {
        Ok(Box::new(MockMidiOutput))
    }
}
