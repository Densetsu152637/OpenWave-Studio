//! cpal hosts: WASAPI (shared), CoreAudio, ALSA, and JACK, PipeWire or ASIO when built in.
//!
//! cpal streams are not `Send` on every platform, so a dedicated thread builds and owns the
//! stream(s) and executes start and stop commands; the [`ThreadedStream`] handle is `Send`.
//! An optional input device runs as a second cpal stream that feeds the output callback through
//! a wait-free ring, since cpal has no duplex stream.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, ErrorKind, FromSample, HostId, Sample as _, SampleFormat, SizedSample, Stream,
    StreamConfig, SupportedStreamConfig,
};

use crate::backend::{
    AudioBackend, AudioCallback, AudioStream, CallbackFactory, DEFAULT_DEVICE, DeviceInfo, IoError,
    IoErrorKind, STANDARD_RATES, StreamInfo, StreamRequest, StreamState,
};
use crate::threaded::{Command, ThreadedStream};

/// Samples per scratch chunk (output conversion and input mixing).
const SCRATCH_SAMPLES: usize = 16_384;
/// Seconds of input audio the capture ring can hold.
const INPUT_RING_SECONDS: f32 = 0.25;

/// One cpal host (for example WASAPI or CoreAudio) as an [`AudioBackend`].
pub struct CpalBackend {
    host: HostId,
}

impl CpalBackend {
    /// Every cpal host available on this platform, default host first.
    pub fn available() -> Vec<CpalBackend> {
        let default = cpal::default_host().id();
        let mut hosts = cpal::available_hosts();
        hosts.sort_by_key(|host| *host != default);
        hosts.into_iter().map(|host| CpalBackend { host }).collect()
    }

    pub fn host_name(&self) -> &'static str {
        self.host.name()
    }
}

fn host_label(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "wasapi" => "WASAPI (shared)".into(),
        "asio" => "ASIO".into(),
        "coreaudio" => "CoreAudio".into(),
        "alsa" => "ALSA".into(),
        "jack" => "JACK".into(),
        "pipewire" => "PipeWire".into(),
        "pulseaudio" => "PulseAudio".into(),
        other => other.to_string(),
    }
}

fn map_error(context: &str, error: &cpal::Error) -> IoError {
    let kind = match error.kind() {
        ErrorKind::DeviceNotAvailable | ErrorKind::HostUnavailable => IoErrorKind::DeviceLost,
        ErrorKind::DeviceBusy => IoErrorKind::Busy,
        ErrorKind::UnsupportedConfig | ErrorKind::InvalidInput => IoErrorKind::Unsupported,
        _ => IoErrorKind::Other,
    };
    IoError::with_kind(kind, context, error)
}

fn device_name<D: DeviceTrait>(device: &D) -> String {
    device
        .description()
        .map(|description| description.name().to_string())
        .unwrap_or_else(|_| device.to_string())
}

fn device_id<D: DeviceTrait>(device: &D) -> String {
    device
        .id()
        .map(|id| id.to_string())
        .unwrap_or_else(|_| device_name(device))
}

impl AudioBackend for CpalBackend {
    fn id(&self) -> String {
        format!("cpal:{}", self.host.name().to_ascii_lowercase())
    }

    fn label(&self) -> String {
        host_label(self.host.name())
    }

    fn devices(&self) -> Result<Vec<DeviceInfo>, IoError> {
        let host = cpal::host_from_id(self.host).map_err(|e| map_error("opening the host", &e))?;
        let default_out = host.default_output_device().map(|d| device_id(&d));
        let default_in = host.default_input_device().map(|d| device_id(&d));
        let devices = host
            .devices()
            .map_err(|e| map_error("listing devices", &e))?;
        let mut list = Vec::new();
        for device in devices {
            let id = device_id(&device);
            let mut sample_rates: Vec<u32> = Vec::new();
            let mut max_out = 0_u16;
            let mut buffers: Option<(u32, u32)> = None;
            if let Ok(configs) = device.supported_output_configs() {
                for config in configs {
                    max_out = max_out.max(config.channels());
                    for rate in STANDARD_RATES {
                        if config.contains_rate(rate) && !sample_rates.contains(&rate) {
                            sample_rates.push(rate);
                        }
                    }
                    if let cpal::SupportedBufferSize::Range { min, max } = config.buffer_size() {
                        buffers = Some(match buffers {
                            Some((lo, hi)) => (lo.min(*min), hi.max(*max)),
                            None => (*min, *max),
                        });
                    }
                }
            }
            let mut max_in = 0_u16;
            if let Ok(configs) = device.supported_input_configs() {
                for config in configs {
                    max_in = max_in.max(config.channels());
                }
            }
            if max_out == 0 && max_in == 0 {
                continue;
            }
            sample_rates.sort_unstable();
            list.push(DeviceInfo {
                is_default_output: default_out.as_deref() == Some(id.as_str()),
                is_default_input: default_in.as_deref() == Some(id.as_str()),
                name: device_name(&device),
                id,
                max_output_channels: max_out,
                max_input_channels: max_in,
                sample_rates,
                buffer_frames: buffers,
            });
        }
        Ok(list)
    }

    fn open(
        &self,
        request: &StreamRequest,
        factory: CallbackFactory,
    ) -> Result<Box<dyn AudioStream>, IoError> {
        let host_id = self.host;
        let backend_id = self.id();
        let request = request.clone();
        let state = Arc::new(StreamState::default());
        let thread_state = Arc::clone(&state);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<StreamInfo, IoError>>();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let thread = thread::Builder::new()
            .name("ow-audio-stream".into())
            .spawn(move || {
                match build_streams(host_id, &backend_id, &request, factory, &thread_state) {
                    Ok((streams, info)) => {
                        let _ = ready_tx.send(Ok(info));
                        run_commands(&streams, &thread_state, &cmd_rx);
                        // Dropping the streams closes the device.
                        drop(streams);
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

struct Streams {
    output: Stream,
    input: Option<Stream>,
}

fn run_commands(streams: &Streams, state: &StreamState, commands: &mpsc::Receiver<Command>) {
    // Ends when the handle is dropped (sender closes).
    while let Ok(command) = commands.recv() {
        match command {
            Command::Start(reply) => {
                let result = (|| {
                    if let Some(input) = &streams.input {
                        input.play().map_err(|e| map_error("starting input", &e))?;
                    }
                    streams
                        .output
                        .play()
                        .map_err(|e| map_error("starting the output stream", &e))
                })();
                if result.is_ok() {
                    state.set_running(true);
                }
                let _ = reply.send(result);
            }
            Command::Stop(reply) => {
                let result = (|| {
                    streams
                        .output
                        .pause()
                        .map_err(|e| map_error("pausing the output stream", &e))?;
                    if let Some(input) = &streams.input {
                        input.pause().map_err(|e| map_error("pausing input", &e))?;
                    }
                    Ok(())
                })();
                if result.is_ok() {
                    state.set_running(false);
                }
                let _ = reply.send(result);
            }
        }
    }
}

/// Finds a device by stored id, then by name.
fn find_device(host: &cpal::Host, wanted: &str, input: bool) -> Option<cpal::Device> {
    if let Ok(id) = wanted.parse::<cpal::DeviceId>()
        && let Some(device) = host.device_by_id(&id)
    {
        return Some(device);
    }
    let devices = host.devices().ok()?;
    devices
        .filter(|d| {
            if input {
                d.supports_input()
            } else {
                d.supports_output()
            }
        })
        .find(|d| device_name(d) == wanted)
}

fn resolve_device(
    host: &cpal::Host,
    wanted: Option<&str>,
    input: bool,
) -> Result<cpal::Device, IoError> {
    let direction = if input { "input" } else { "output" };
    match wanted {
        None | Some(DEFAULT_DEVICE) => {
            let default = if input {
                host.default_input_device()
            } else {
                host.default_output_device()
            };
            default.ok_or_else(|| {
                IoError::with_kind(
                    IoErrorKind::NotFound,
                    "opening the default device",
                    format_args!("no {direction} device"),
                )
            })
        }
        Some(id) => find_device(host, id, input).ok_or_else(|| {
            IoError::with_kind(
                IoErrorKind::NotFound,
                "opening the device",
                format_args!("{direction} device \"{id}\" not found"),
            )
        }),
    }
}

/// Picks the format: the device default, adjusted to the requested rate and channel count when
/// a supported range allows it.
fn pick_config(
    device: &cpal::Device,
    request: &StreamRequest,
    input: bool,
) -> Result<SupportedStreamConfig, IoError> {
    let default = if input {
        device.default_input_config()
    } else {
        device.default_output_config()
    }
    .map_err(|e| map_error("reading the default format", &e))?;
    let channels = if input { None } else { request.output_channels };
    if request.sample_rate.is_none() && channels.is_none() {
        return Ok(default);
    }
    let rate = request.sample_rate.unwrap_or_else(|| default.sample_rate());
    let wanted_channels = channels.unwrap_or_else(|| default.channels());
    let ranges: Vec<_> = if input {
        device
            .supported_input_configs()
            .map_err(|e| map_error("reading supported formats", &e))?
            .collect()
    } else {
        device
            .supported_output_configs()
            .map_err(|e| map_error("reading supported formats", &e))?
            .collect()
    };
    let preferred = ranges
        .iter()
        .filter(|range| range.channels() == wanted_channels && range.contains_rate(rate))
        .min_by_key(|range| u8::from(range.sample_format() != default.sample_format()));
    preferred
        .and_then(|range| (*range).try_with_sample_rate(rate))
        .ok_or_else(|| {
            IoError::with_kind(
                IoErrorKind::Unsupported,
                "choosing the stream format",
                format_args!(
                    "the device does not support {rate} Hz with {wanted_channels} channels"
                ),
            )
        })
}

fn build_streams(
    host_id: HostId,
    backend_id: &str,
    request: &StreamRequest,
    factory: CallbackFactory,
    state: &Arc<StreamState>,
) -> Result<(Streams, StreamInfo), IoError> {
    let host = cpal::host_from_id(host_id).map_err(|e| map_error("opening the host", &e))?;
    let out_device = resolve_device(&host, request.output_device.as_deref(), false)?;
    let out_supported = pick_config(&out_device, request, false)?;
    let out_format = out_supported.sample_format();
    let mut config: StreamConfig = out_supported.into();
    if let Some(frames) = request.buffer_frames
        && let cpal::SupportedBufferSize::Range { min, max } = out_supported.buffer_size()
    {
        config.buffer_size = BufferSize::Fixed(frames.clamp(*min, *max));
    }

    // Input: a second stream at the output's sample rate, feeding a ring.
    let mut input_parts = None;
    let mut input_channels = 0_u16;
    if let Some(wanted) = request.input_device.as_deref() {
        let in_device = resolve_device(&host, Some(wanted), true)?;
        let in_request = StreamRequest {
            sample_rate: Some(config.sample_rate),
            ..StreamRequest::default()
        };
        let in_supported = pick_config(&in_device, &in_request, true)?;
        input_channels = in_supported.channels();
        input_parts = Some((in_device, in_supported));
    }

    let mut info = StreamInfo {
        backend: backend_id.to_string(),
        device_id: device_id(&out_device),
        device_name: device_name(&out_device),
        sample_rate: config.sample_rate,
        output_channels: config.channels,
        input_channels,
        buffer_frames: match config.buffer_size {
            BufferSize::Fixed(frames) => Some(frames),
            BufferSize::Default => None,
        },
        exclusive: false,
    };

    let mut input_ring = None;
    let mut input_stream = None;
    if let Some((device, supported)) = input_parts {
        let capacity =
            (config.sample_rate as f32 * f32::from(input_channels) * INPUT_RING_SECONDS) as usize;
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(capacity.max(1024));
        let in_format = supported.sample_format();
        let mut in_config: StreamConfig = supported.into();
        in_config.buffer_size = config.buffer_size;
        let stream = match in_format {
            SampleFormat::F32 => build_input::<f32>(&device, in_config, producer, state),
            SampleFormat::I16 => build_input::<i16>(&device, in_config, producer, state),
            SampleFormat::I32 => build_input::<i32>(&device, in_config, producer, state),
            SampleFormat::U16 => build_input::<u16>(&device, in_config, producer, state),
            other => Err(IoError::with_kind(
                IoErrorKind::Unsupported,
                "opening the input",
                format_args!("unsupported sample format {other}"),
            )),
        }?;
        input_stream = Some(stream);
        input_ring = Some(consumer);
    }

    let callback = factory(&info);
    let output = match out_format {
        SampleFormat::F32 => build_output::<f32>(
            &out_device,
            config,
            callback,
            input_ring,
            input_channels,
            state,
        ),
        SampleFormat::I16 => build_output::<i16>(
            &out_device,
            config,
            callback,
            input_ring,
            input_channels,
            state,
        ),
        SampleFormat::I32 => build_output::<i32>(
            &out_device,
            config,
            callback,
            input_ring,
            input_channels,
            state,
        ),
        SampleFormat::U16 => build_output::<u16>(
            &out_device,
            config,
            callback,
            input_ring,
            input_channels,
            state,
        ),
        other => Err(IoError::with_kind(
            IoErrorKind::Unsupported,
            "opening the output",
            format_args!("unsupported sample format {other}"),
        )),
    }?;
    if info.buffer_frames.is_none() {
        info.buffer_frames = output.buffer_size().ok();
    }
    Ok((
        Streams {
            output,
            input: input_stream,
        },
        info,
    ))
}

fn error_handler(
    state: &Arc<StreamState>,
    what: &'static str,
) -> impl FnMut(cpal::Error) + Send + 'static {
    let state = Arc::clone(state);
    move |error: cpal::Error| {
        eprintln!("[ow-io] {what} stream error: {error}");
        match error.kind() {
            ErrorKind::DeviceNotAvailable | ErrorKind::HostUnavailable => {
                state.fail(true, format!("{what}: {error}"));
            }
            ErrorKind::StreamInvalidated => state.fail(false, format!("{what}: {error}")),
            _ => {}
        }
    }
}

fn build_input<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut producer: rtrb::Producer<f32>,
    state: &Arc<StreamState>,
) -> Result<Stream, IoError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                // A full ring drops the newest samples; the output side trims latency.
                for sample in data {
                    if producer.push(f32::from_sample(*sample)).is_err() {
                        break;
                    }
                }
            },
            error_handler(state, "input"),
            None,
        )
        .map_err(|e| map_error("building the input stream", &e))
}

fn build_output<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut callback: AudioCallback,
    mut input: Option<rtrb::Consumer<f32>>,
    input_channels: u16,
    state: &Arc<StreamState>,
) -> Result<Stream, IoError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels).max(1);
    let in_channels = usize::from(input_channels);
    // Allocated here, before the stream starts, never inside the callback.
    let mut scratch = vec![0.0_f32; SCRATCH_SAMPLES];
    let mut input_scratch = vec![0.0_f32; if in_channels > 0 { SCRATCH_SAMPLES } else { 0 }];
    let chunk_frames = (SCRATCH_SAMPLES / channels.max(in_channels).max(1)).max(1);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let mut offset = 0;
                while offset < data.len() {
                    let frames = chunk_frames.min((data.len() - offset) / channels);
                    if frames == 0 {
                        break;
                    }
                    let out = &mut data[offset..offset + frames * channels];
                    let buf = &mut scratch[..frames * channels];
                    buf.fill(0.0);
                    match input.as_mut() {
                        Some(ring) => {
                            let wanted = frames * in_channels;
                            let capture = &mut input_scratch[..wanted];
                            // Cap latency: drop input beyond four callbacks of audio.
                            let excess = ring.slots().saturating_sub(wanted * 4);
                            for _ in 0..excess {
                                let _ = ring.pop();
                            }
                            for slot in capture.iter_mut() {
                                *slot = ring.pop().unwrap_or(0.0);
                            }
                            callback(buf, Some(capture));
                        }
                        None => callback(buf, None),
                    }
                    for (dst, src) in out.iter_mut().zip(buf.iter()) {
                        *dst = T::from_sample(*src);
                    }
                    offset += frames * channels;
                }
            },
            error_handler(state, "output"),
            None,
        )
        .map_err(|e| map_error("building the output stream", &e))
}
