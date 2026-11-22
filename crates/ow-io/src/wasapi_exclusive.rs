//! WASAPI exclusive mode (Windows), through the `wasapi` crate.
//!
//! cpal's WASAPI host runs in shared mode, where Windows mixes and resamples every application's
//! audio and adds latency. Exclusive mode talks to the endpoint directly with an event-driven
//! buffer of a chosen size. The device must support the exact format, so the backend tries a
//! list of rate, channel and sample-format combinations and reports what it opened.
//!
//! The stream thread initialises COM, promotes itself to real-time priority, and runs the audio
//! callback straight from the render event. Output only: an input device in the request is
//! rejected with a clear error (use a shared host for recording until a later stage adds
//! exclusive capture).

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use wasapi::{
    AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode, WasapiError, WaveFormat,
    calculate_period_100ns,
};

use crate::backend::{
    AudioBackend, AudioCallback, AudioStream, CallbackFactory, DEFAULT_DEVICE, DeviceInfo, IoError,
    IoErrorKind, MAX_BUFFER_FRAMES, STANDARD_RATES, StreamInfo, StreamRequest, StreamState,
};
use crate::threaded::{Command, ThreadedStream};

/// Backend id of the exclusive-mode backend.
pub const WASAPI_EXCLUSIVE_ID: &str = "wasapi-exclusive";

// HRESULTs from audioclient.h that need distinct handling.
const E_BUFFER_SIZE_NOT_ALIGNED: u32 = 0x8889_0019;
const E_DEVICE_IN_USE: u32 = 0x8889_000A;
const E_EXCLUSIVE_NOT_ALLOWED: u32 = 0x8889_000E;
const E_UNSUPPORTED_FORMAT: u32 = 0x8889_0008;
const E_DEVICE_INVALIDATED: u32 = 0x8889_0004;

/// Wait for a render event for this long before treating the device as gone.
const EVENT_TIMEOUT_MS: u32 = 1_000;
/// Period used when no buffer size is requested.
const DEFAULT_BUFFER_FRAMES: u32 = 256;

/// WASAPI exclusive mode as an [`AudioBackend`].
#[derive(Debug, Default)]
pub struct WasapiExclusiveBackend;

/// Sample encodings tried, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    F32,
    I32,
    /// 24 valid bits in a 32-bit container.
    I24In32,
    /// Packed 24-bit.
    I24,
    I16,
}

impl Encoding {
    const ALL: [Encoding; 5] = [
        Encoding::F32,
        Encoding::I32,
        Encoding::I24In32,
        Encoding::I24,
        Encoding::I16,
    ];

    fn format(self, rate: usize, channels: usize) -> WaveFormat {
        let (store, valid, kind) = match self {
            Encoding::F32 => (32, 32, SampleType::Float),
            Encoding::I32 => (32, 32, SampleType::Int),
            Encoding::I24In32 => (32, 24, SampleType::Int),
            Encoding::I24 => (24, 24, SampleType::Int),
            Encoding::I16 => (16, 16, SampleType::Int),
        };
        WaveFormat::new(store, valid, kind, rate, channels, None)
    }

    fn name(self) -> &'static str {
        match self {
            Encoding::F32 => "32-bit float",
            Encoding::I32 => "32-bit integer",
            Encoding::I24In32 => "24-bit (32-bit container)",
            Encoding::I24 => "24-bit",
            Encoding::I16 => "16-bit",
        }
    }

    fn bytes_per_sample(self) -> usize {
        match self {
            Encoding::F32 | Encoding::I32 | Encoding::I24In32 => 4,
            Encoding::I24 => 3,
            Encoding::I16 => 2,
        }
    }

    /// Converts interleaved `f32` samples (already finite and clamped by the engine) to bytes.
    fn encode(self, input: &[f32], out: &mut [u8]) {
        match self {
            Encoding::F32 => {
                for (sample, bytes) in input.iter().zip(out.as_chunks_mut::<4>().0) {
                    *bytes = sample.to_le_bytes();
                }
            }
            Encoding::I32 => {
                for (sample, bytes) in input.iter().zip(out.as_chunks_mut::<4>().0) {
                    let value = (f64::from(sample.clamp(-1.0, 1.0)) * 2_147_483_647.0) as i32;
                    *bytes = value.to_le_bytes();
                }
            }
            Encoding::I24In32 => {
                for (sample, bytes) in input.iter().zip(out.as_chunks_mut::<4>().0) {
                    let value = (f64::from(sample.clamp(-1.0, 1.0)) * 8_388_607.0) as i32;
                    *bytes = (value << 8).to_le_bytes();
                }
            }
            Encoding::I24 => {
                for (sample, bytes) in input.iter().zip(out.as_chunks_mut::<3>().0) {
                    let value = (f64::from(sample.clamp(-1.0, 1.0)) * 8_388_607.0) as i32;
                    bytes.copy_from_slice(&value.to_le_bytes()[..3]);
                }
            }
            Encoding::I16 => {
                for (sample, bytes) in input.iter().zip(out.as_chunks_mut::<2>().0) {
                    let value = (sample.clamp(-1.0, 1.0) * 32_767.0) as i16;
                    *bytes = value.to_le_bytes();
                }
            }
        }
    }
}

fn hresult(error: &WasapiError) -> Option<u32> {
    match error {
        WasapiError::Windows(inner) => Some(inner.code().0 as u32),
        _ => None,
    }
}

fn map_error(context: &str, error: &WasapiError) -> IoError {
    let kind = match hresult(error) {
        Some(E_DEVICE_IN_USE | E_EXCLUSIVE_NOT_ALLOWED) => IoErrorKind::Busy,
        Some(E_UNSUPPORTED_FORMAT) => IoErrorKind::Unsupported,
        Some(E_DEVICE_INVALIDATED) => IoErrorKind::DeviceLost,
        _ => match error {
            WasapiError::DeviceNotFound(_) => IoErrorKind::NotFound,
            WasapiError::UnsupportedFormat => IoErrorKind::Unsupported,
            _ => IoErrorKind::Other,
        },
    };
    let detail = match hresult(error) {
        Some(E_DEVICE_IN_USE) => "the device is in use by another application".to_string(),
        Some(E_EXCLUSIVE_NOT_ALLOWED) => {
            "exclusive mode is disabled for this device in the Windows sound settings".to_string()
        }
        _ => error.to_string(),
    };
    IoError::with_kind(kind, context, detail)
}

/// Runs `work` on a fresh thread so its COM apartment is independent of the caller's (the
/// application's UI thread is single-threaded-apartment COM).
fn on_com_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, IoError> + Send + 'static,
) -> Result<T, IoError> {
    thread::Builder::new()
        .name("ow-wasapi-com".into())
        .spawn(move || {
            let _ = wasapi::initialize_mta();
            work()
        })
        .map_err(|e| IoError::new("spawning a COM thread", e))?
        .join()
        .map_err(|_| IoError::new("WASAPI", "worker thread panicked"))?
}

fn open_device(
    enumerator: &DeviceEnumerator,
    wanted: Option<&str>,
) -> Result<wasapi::Device, IoError> {
    match wanted {
        None | Some(DEFAULT_DEVICE) => enumerator
            .get_default_device(Direction::Render)
            .map_err(|e| map_error("opening the default output", &e)),
        Some(id) => enumerator
            .get_device(id)
            .or_else(|_| {
                // Fall back to a friendly-name match so a changed endpoint id still resolves.
                enumerator
                    .get_device_collection(Direction::Render)
                    .and_then(|collection| collection.get_device_with_name(id))
            })
            .map_err(|e| map_error("opening the output device", &e)),
    }
}

impl AudioBackend for WasapiExclusiveBackend {
    fn id(&self) -> String {
        WASAPI_EXCLUSIVE_ID.into()
    }

    fn label(&self) -> String {
        "WASAPI (exclusive, low latency)".into()
    }

    fn devices(&self) -> Result<Vec<DeviceInfo>, IoError> {
        on_com_thread(|| {
            let enumerator =
                DeviceEnumerator::new().map_err(|e| map_error("opening the device list", &e))?;
            let default_id = enumerator
                .get_default_device(Direction::Render)
                .and_then(|device| device.get_id())
                .ok();
            let collection = enumerator
                .get_device_collection(Direction::Render)
                .map_err(|e| map_error("listing devices", &e))?;
            let count = collection
                .get_nbr_devices()
                .map_err(|e| map_error("listing devices", &e))?;
            let mut list = Vec::new();
            for index in 0..count {
                let Ok(device) = collection.get_device_at_index(index) else {
                    continue;
                };
                let (Ok(id), Ok(name)) = (device.get_id(), device.get_friendlyname()) else {
                    continue;
                };
                let mut rates = Vec::new();
                let mut buffers = None;
                if let Ok(client) = device.get_iaudioclient() {
                    for rate in STANDARD_RATES {
                        let supported = Encoding::ALL.iter().any(|encoding| {
                            client
                                .is_supported_exclusive_with_quirks(
                                    &encoding.format(rate as usize, 2),
                                )
                                .is_ok()
                        });
                        if supported {
                            rates.push(rate);
                        }
                    }
                    if let Ok((_, min)) = client.get_device_period() {
                        let min_frames = (min as f64 * 48_000.0 / 10_000_000.0).ceil() as u32;
                        buffers = Some((min_frames.max(16), MAX_BUFFER_FRAMES));
                    }
                }
                list.push(DeviceInfo {
                    is_default_output: default_id.as_deref() == Some(id.as_str()),
                    is_default_input: false,
                    id,
                    name,
                    max_output_channels: 2,
                    max_input_channels: 0,
                    sample_rates: rates,
                    buffer_frames: buffers,
                });
            }
            Ok(list)
        })
    }

    fn open(
        &self,
        request: &StreamRequest,
        factory: CallbackFactory,
    ) -> Result<Box<dyn AudioStream>, IoError> {
        if request.input_device.is_some() {
            return Err(IoError::with_kind(
                IoErrorKind::Unsupported,
                "opening WASAPI exclusive",
                "input is not supported in exclusive mode; choose a shared host to record",
            ));
        }
        let request = request.clone();
        let state = Arc::new(StreamState::default());
        let thread_state = Arc::clone(&state);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<StreamInfo, IoError>>();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let thread = thread::Builder::new()
            .name("ow-wasapi-exclusive".into())
            .spawn(move || {
                let _ = wasapi::initialize_mta();
                match Opened::new(&request, factory) {
                    Ok(opened) => {
                        let _ = ready_tx.send(Ok(opened.info.clone()));
                        opened.run(&thread_state, &cmd_rx);
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .map_err(|e| IoError::new("spawning the audio stream thread", e))?;
        let info = ready_rx
            .recv()
            .map_err(|_| IoError::new("opening the stream", "stream thread exited"))??;
        Ok(Box::new(ThreadedStream::new(info, state, cmd_tx, thread)))
    }
}

/// An initialised exclusive client, ready to run.
struct Opened {
    client: AudioClient,
    render: wasapi::AudioRenderClient,
    event: wasapi::Handle,
    encoding: Encoding,
    channels: usize,
    buffer_frames: usize,
    callback: AudioCallback,
    info: StreamInfo,
}

impl Opened {
    fn new(request: &StreamRequest, factory: CallbackFactory) -> Result<Self, IoError> {
        let enumerator =
            DeviceEnumerator::new().map_err(|e| map_error("opening the device list", &e))?;
        let device = open_device(&enumerator, request.output_device.as_deref())?;
        let device_id = device.get_id().unwrap_or_default();
        let device_name = device.get_friendlyname().unwrap_or_default();
        let mut client = device
            .get_iaudioclient()
            .map_err(|e| map_error("opening the audio client", &e))?;

        // Candidate formats: requested values first, then the device's mix format and 48 kHz.
        let mix = client.get_mixformat().ok();
        let mix_rate = mix.as_ref().map(|f| f.get_samplespersec() as usize);
        let mix_channels = mix.as_ref().map(|f| usize::from(f.get_nchannels()));
        let mut rates: Vec<usize> = Vec::new();
        match request.sample_rate {
            Some(rate) => rates.push(rate as usize),
            None => {
                rates.extend(mix_rate);
                rates.extend([48_000, 44_100, 96_000, 88_200, 192_000]);
            }
        }
        rates.dedup();
        let mut channel_options: Vec<usize> = Vec::new();
        match request.output_channels {
            Some(channels) => channel_options.push(usize::from(channels)),
            None => {
                channel_options.push(2);
                channel_options.extend(mix_channels);
            }
        }
        channel_options.dedup();

        let mut chosen = None;
        'search: for &rate in &rates {
            for &channels in &channel_options {
                for encoding in Encoding::ALL {
                    if let Ok(format) =
                        client.is_supported_exclusive_with_quirks(&encoding.format(rate, channels))
                    {
                        chosen = Some((format, encoding, rate, channels));
                        break 'search;
                    }
                }
            }
        }
        let Some((format, encoding, rate, channels)) = chosen else {
            return Err(IoError::with_kind(
                IoErrorKind::Unsupported,
                "opening WASAPI exclusive",
                "the device accepts none of the tried exclusive-mode formats \
                 (is exclusive mode enabled in the Windows sound settings?)",
            ));
        };

        let wanted_frames = request
            .buffer_frames
            .unwrap_or(DEFAULT_BUFFER_FRAMES)
            .max(16);
        let desired = calculate_period_100ns(i64::from(wanted_frames), rate as i64);
        let mut period = client
            .calculate_aligned_period_near(desired, Some(128), &format)
            .map_err(|e| map_error("choosing the buffer size", &e))?;
        let mut init = client.initialize_client(
            &format,
            Direction::Render,
            StreamMode::EventsExclusive { period_hns: period },
            None,
        );
        if let Err(error) = &init
            && hresult(error) == Some(E_BUFFER_SIZE_NOT_ALIGNED)
        {
            // The documented recovery: ask for the next aligned size and start again with a
            // fresh client.
            let aligned = client
                .get_buffer_size()
                .map_err(|e| map_error("aligning the buffer size", &e))?;
            period = calculate_period_100ns(i64::from(aligned), rate as i64);
            client = device
                .get_iaudioclient()
                .map_err(|e| map_error("opening the audio client", &e))?;
            init = client.initialize_client(
                &format,
                Direction::Render,
                StreamMode::EventsExclusive { period_hns: period },
                None,
            );
        }
        init.map_err(|e| map_error("initialising exclusive mode", &e))?;
        let _ = period;

        let buffer_frames = client
            .get_buffer_size()
            .map_err(|e| map_error("reading the buffer size", &e))?
            as usize;
        let event = client
            .set_get_eventhandle()
            .map_err(|e| map_error("creating the render event", &e))?;
        let render = client
            .get_audiorenderclient()
            .map_err(|e| map_error("opening the render client", &e))?;

        let info = StreamInfo {
            backend: WASAPI_EXCLUSIVE_ID.into(),
            device_id,
            device_name: format!("{device_name} [{}]", encoding.name()),
            sample_rate: rate as u32,
            output_channels: channels as u16,
            input_channels: 0,
            buffer_frames: Some(buffer_frames as u32),
            exclusive: true,
        };
        let callback = factory(&info);
        Ok(Self {
            client,
            render,
            event,
            encoding,
            channels,
            buffer_frames,
            callback,
            info,
        })
    }

    fn run(mut self, state: &StreamState, commands: &Receiver<Command>) {
        // Preallocated: the render loop never allocates.
        let mut samples = vec![0.0_f32; self.buffer_frames * self.channels];
        let mut bytes = vec![0_u8; samples.len() * self.encoding.bytes_per_sample()];
        let _priority = audio_thread_priority::promote_current_thread_to_real_time(
            self.buffer_frames as u32,
            self.info.sample_rate,
        );

        loop {
            // Idle until started; the channel closing ends the thread.
            let Ok(command) = commands.recv() else {
                return;
            };
            match command {
                Command::Start(reply) => {
                    // Fill the first buffer before starting, as the WASAPI docs require.
                    let started = self
                        .fill(&mut samples, &mut bytes, self.buffer_frames)
                        .and_then(|()| {
                            self.client
                                .start_stream()
                                .map_err(|e| map_error("starting", &e))
                        });
                    match started {
                        Ok(()) => {
                            state.set_running(true);
                            let _ = reply.send(Ok(()));
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                    }
                }
                Command::Stop(reply) => {
                    let _ = reply.send(Ok(()));
                    continue;
                }
            }
            if !self.run_started(state, commands, &mut samples, &mut bytes) {
                return;
            }
        }
    }

    /// Renders until stopped. Returns false when the thread should end (failure or close).
    fn run_started(
        &mut self,
        state: &StreamState,
        commands: &Receiver<Command>,
        samples: &mut [f32],
        bytes: &mut [u8],
    ) -> bool {
        loop {
            match commands.try_recv() {
                Ok(Command::Stop(reply)) => {
                    let result = self
                        .client
                        .stop_stream()
                        .map_err(|e| map_error("stopping", &e));
                    if result.is_ok() {
                        state.set_running(false);
                    }
                    let _ = reply.send(result);
                    return true;
                }
                Ok(Command::Start(reply)) => {
                    let _ = reply.send(Ok(()));
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    let _ = self.client.stop_stream();
                    return false;
                }
            }
            if self.event.wait_for_event(EVENT_TIMEOUT_MS).is_err() {
                state.fail(true, "the audio device stopped responding");
                return false;
            }
            let frames = match self.client.get_available_space_in_frames() {
                Ok(frames) => frames as usize,
                Err(error) => {
                    state.fail(true, format!("reading the device buffer: {error}"));
                    return false;
                }
            };
            if let Err(error) = self.fill(samples, bytes, frames.min(self.buffer_frames)) {
                state.fail(true, error.to_string());
                return false;
            }
        }
    }

    /// Runs the callback for `frames` frames and writes them to the device.
    fn fill(
        &mut self,
        samples: &mut [f32],
        bytes: &mut [u8],
        frames: usize,
    ) -> Result<(), IoError> {
        if frames == 0 {
            return Ok(());
        }
        let count = frames * self.channels;
        let block = &mut samples[..count];
        block.fill(0.0);
        (self.callback)(block, None);
        let size = count * self.encoding.bytes_per_sample();
        self.encoding.encode(block, &mut bytes[..size]);
        self.render
            .write_to_device(&bytes[..size], None)
            .map_err(|e| map_error("writing to the device", &e))
    }
}
