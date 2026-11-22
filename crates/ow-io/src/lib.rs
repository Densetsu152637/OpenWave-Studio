//! Audio device backends for OpenWave Studio.
//!
//! Stage 1 provides one backend: the default output device through `cpal`. The render
//! callback is generic over the host sample format; conversion from the engine's interleaved
//! `f32` buffers uses a scratch buffer allocated before the stream starts, so the audio
//! callback itself never allocates.

use std::fmt;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};

/// Frames per scratch chunk when the host sample format is not `f32`.
const SCRATCH_SAMPLES: usize = 16_384;

/// Failure to open or run an audio device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoError(String);

impl IoError {
    fn new(context: &str, cause: impl fmt::Display) -> Self {
        Self(format!("{context}: {cause}"))
    }
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for IoError {}

/// What was actually opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputInfo {
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

/// A running output stream. Dropping it stops the stream.
///
/// `cpal` streams are not `Send` on every platform, so a dedicated thread owns the stream and
/// this handle (which is `Send + Sync`) only holds the shutdown channel.
pub struct OutputStream {
    info: OutputInfo,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl OutputStream {
    /// Opens the default output device and starts it.
    ///
    /// `make_render` runs once on the stream thread with the opened device's format and returns
    /// the render callback. The callback receives an interleaved `f32` buffer and the channel
    /// count and must not allocate, lock or block.
    pub fn open_default<F, R>(make_render: F) -> Result<Self, IoError>
    where
        F: FnOnce(&OutputInfo) -> R + Send + 'static,
        R: FnMut(&mut [f32], usize) + Send + 'static,
    {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<OutputInfo, IoError>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let thread = thread::Builder::new()
            .name("ow-audio-stream".into())
            .spawn(move || match start_stream(make_render) {
                Ok((stream, info)) => {
                    let _ = ready_tx.send(Ok(info));
                    // Blocks until the handle is dropped (sender closes) or sends a stop.
                    let _ = stop_rx.recv();
                    drop(stream);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|error| IoError::new("spawning the audio stream thread", error))?;
        let info = ready_rx
            .recv()
            .map_err(|_| IoError::new("starting the audio stream", "stream thread exited"))??;
        Ok(Self {
            info,
            stop: Some(stop_tx),
            thread: Some(thread),
        })
    }

    pub fn info(&self) -> &OutputInfo {
        &self.info
    }
}

impl Drop for OutputStream {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn start_stream<F, R>(make_render: F) -> Result<(Stream, OutputInfo), IoError>
where
    F: FnOnce(&OutputInfo) -> R,
    R: FnMut(&mut [f32], usize) + Send + 'static,
{
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| IoError::new("opening the default output", "no output device"))?;
    let supported = device
        .default_output_config()
        .map_err(|error| IoError::new("reading the default output format", error))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let info = OutputInfo {
        device_name: device.to_string(),
        sample_rate: config.sample_rate,
        channels: config.channels,
    };
    let render = make_render(&info);
    let stream = match format {
        SampleFormat::F32 => build_f32(&device, config, render),
        SampleFormat::I16 => build_converted::<i16, R>(&device, config, render),
        SampleFormat::I32 => build_converted::<i32, R>(&device, config, render),
        SampleFormat::U16 => build_converted::<u16, R>(&device, config, render),
        other => Err(IoError::new(
            "opening the default output",
            format_args!("unsupported sample format {other}"),
        )),
    }?;
    stream
        .play()
        .map_err(|error| IoError::new("starting the output stream", error))?;
    Ok((stream, info))
}

fn on_stream_error(error: cpal::Error) {
    eprintln!("[ow-io] output stream error: {error}");
}

/// Host buffers are already `f32`: the engine renders straight into them.
fn build_f32<R>(
    device: &cpal::Device,
    config: StreamConfig,
    mut render: R,
) -> Result<Stream, IoError>
where
    R: FnMut(&mut [f32], usize) + Send + 'static,
{
    let channels = usize::from(config.channels).max(1);
    device
        .build_output_stream(
            config,
            move |data: &mut [f32], _| {
                data.fill(0.0);
                render(data, channels);
            },
            on_stream_error,
            None,
        )
        .map_err(|error| IoError::new("building the output stream", error))
}

/// Host buffers use another sample format: render into a preallocated `f32` scratch buffer
/// (in whole-frame chunks) and convert.
fn build_converted<T, R>(
    device: &cpal::Device,
    config: StreamConfig,
    mut render: R,
) -> Result<Stream, IoError>
where
    T: SizedSample + FromSample<f32>,
    R: FnMut(&mut [f32], usize) + Send + 'static,
{
    let channels = usize::from(config.channels).max(1);
    // Allocated here, before the stream starts, never inside the callback.
    let mut scratch = vec![0.0_f32; SCRATCH_SAMPLES];
    let chunk = (SCRATCH_SAMPLES / channels).max(1) * channels;
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                for out in data.chunks_mut(chunk) {
                    let buf = &mut scratch[..out.len()];
                    buf.fill(0.0);
                    render(buf, channels);
                    for (dst, src) in out.iter_mut().zip(buf.iter()) {
                        *dst = T::from_sample(*src);
                    }
                }
            },
            on_stream_error,
            None,
        )
        .map_err(|error| IoError::new("building the output stream", error))
}
