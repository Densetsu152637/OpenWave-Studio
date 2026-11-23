//! Audio recording: the record tap that runs on the audio thread and the control-thread
//! [`Recording`] that owns the take files.
//!
//! A recording is one [`RecordSet`]: a [`RecordProducer`] per armed track, handed to the audio
//! thread through its own wait-free ring ([`ControlLink::swap_record`](crate::ControlLink)) and
//! handed back the same way ([`ControlLink::collect_retired`](crate::ControlLink)), so the audio
//! thread never frees a ring. While the transport is recording, [`AudioCore::process`] pushes
//! the device's input channels (mono: channel 0; stereo and wider: channels 0 and 1) into every
//! producer with no allocation, lock or I/O; a writer thread per take (`ow-media`) turns the
//! rings into WAV files.
//!
//! Placement. A sound the engine plays is heard back on the input one round trip
//! (`latency_frames`, input plus output) later, so input frame `i` of a recording that began at
//! timeline position `P` belongs to position `P + i - latency`. A take therefore starts at
//! `P - latency`; when that is before the timeline start, the take's first `latency - P` frames
//! (which belong to negative positions) are dropped and it starts at 0.
//!
//! Stopping: [`Engine::stop_recording`](crate::Engine::stop_recording) stops the transport and
//! sends `EndRecording`; the audio thread marks the set finished (it never pushes again) and
//! retires it, and only then are the writers finished, so every frame the tap pushed is on disk
//! (the take length equals the frames recorded, see [`RecordShared::captured`]).
//!
//! Not recorded: input monitoring, punch ranges, loop takes, count-in audio.
//!
//! [`AudioCore::process`]: crate::AudioCore::process

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ow_media::{RecordProducer, RecordWriter};
use ow_model::TrackId;

use crate::graph::MAX_BLOCK;

/// Seconds of audio each take's ring holds before the writer thread must have drained it.
pub const RING_SECONDS: u32 = 2;

/// What the tap and the control side both see; the audio thread only does relaxed atomic
/// stores here.
#[derive(Debug, Default)]
pub struct RecordShared {
    started: AtomicBool,
    start_sample: AtomicU64,
    captured: AtomicU64,
    done: AtomicBool,
}

impl RecordShared {
    /// True once the tap has captured its first frame.
    pub fn started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    /// Timeline sample at which the takes start (recording start minus the round-trip latency,
    /// at least 0); meaningful once [`started`](Self::started).
    pub fn start_sample(&self) -> u64 {
        self.start_sample.load(Ordering::Acquire)
    }

    /// Frames the tap pushed to each take's ring so far, after dropping the leading frames that
    /// belong before the timeline start. Each take file holds exactly this many frames once
    /// finished, less whatever its ring had no room for ([`FinishedTake::dropped_frames`]).
    pub fn captured(&self) -> u64 {
        self.captured.load(Ordering::Acquire)
    }

    /// True once the audio thread will push no more frames for this recording.
    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

/// The audio-thread half of a recording: one producer per take plus the placement rules.
pub struct RecordSet {
    producers: Vec<RecordProducer>,
    /// Channels in each take (1 or 2).
    channels: usize,
    latency: u64,
    /// Leading frames still to drop (those that belong before the timeline start).
    skip: u64,
    scratch: Vec<f32>,
    ended: bool,
    shared: Arc<RecordShared>,
}

impl RecordSet {
    /// Builds the set for `producers` (all created with `channels` channels) and the round-trip
    /// `latency_frames`. The shared state is how the control side watches the recording.
    pub fn new(
        producers: Vec<RecordProducer>,
        channels: u16,
        latency_frames: u32,
    ) -> (Self, Arc<RecordShared>) {
        let channels = usize::from(channels.clamp(1, 2));
        let shared = Arc::new(RecordShared::default());
        let set = Self {
            producers,
            channels,
            latency: u64::from(latency_frames),
            skip: 0,
            scratch: vec![0.0; MAX_BLOCK * channels],
            ended: false,
            shared: Arc::clone(&shared),
        };
        (set, shared)
    }

    pub(crate) fn is_ended(&self) -> bool {
        self.ended
    }

    /// Marks the recording finished: no more frames are pushed from here on.
    pub(crate) fn end(&mut self) {
        self.ended = true;
        self.shared.done.store(true, Ordering::Release);
    }

    /// Pushes this cycle's interleaved `input` (`in_channels` channels per frame) to every take.
    /// `position` is the timeline position at the start of the cycle. Audio thread; allocation
    /// free.
    pub(crate) fn capture(&mut self, position: u64, input: &[f32], in_channels: usize) {
        if self.ended || in_channels < self.channels {
            return;
        }
        if !self.shared.started.load(Ordering::Relaxed) {
            self.skip = self.latency.saturating_sub(position);
            self.shared
                .start_sample
                .store(position.saturating_sub(self.latency), Ordering::Release);
            self.shared.started.store(true, Ordering::Release);
        }
        let frames = input.len() / in_channels;
        let dropped = self.skip.min(frames as u64) as usize;
        self.skip -= dropped as u64;
        let Self {
            producers,
            channels,
            scratch,
            shared,
            ..
        } = self;
        let channels = *channels;
        let mut at = dropped;
        while at < frames {
            let count = (frames - at).min(MAX_BLOCK);
            let source = &input[at * in_channels..(at + count) * in_channels];
            let block = if in_channels == channels {
                source
            } else {
                for (frame, out) in source
                    .chunks_exact(in_channels)
                    .zip(scratch.chunks_exact_mut(channels))
                {
                    out.copy_from_slice(&frame[..channels]);
                }
                &scratch[..count * channels]
            };
            for producer in producers.iter_mut() {
                producer.push(block);
            }
            shared.captured.fetch_add(count as u64, Ordering::Release);
            at += count;
        }
    }
}

/// Why a recording could not start or stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    /// No audio stream is open.
    NoStream,
    /// The open stream has no input device.
    NoInput,
    /// No take was requested.
    NoTakes,
    /// A recording is already running on this stream.
    Busy,
    /// A take file could not be created or finished.
    Io(String),
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoStream => f.write_str("the audio engine is not running"),
            Self::NoInput => {
                f.write_str("no audio input is open; choose an input device in Settings")
            }
            Self::NoTakes => f.write_str("no track is armed for recording"),
            Self::Busy => f.write_str("the audio engine is busy; try again"),
            Self::Io(message) => write!(f, "could not write the recording: {message}"),
        }
    }
}

impl std::error::Error for RecordError {}

struct TakeWriter {
    track: TrackId,
    path: PathBuf,
    writer: RecordWriter,
}

/// A running recording (control thread): the take files being written, one per armed track.
/// Give it back to [`Engine::stop_recording`](crate::Engine::stop_recording) to finish it.
pub struct Recording {
    pub(crate) shared: Arc<RecordShared>,
    writers: Vec<TakeWriter>,
    sample_rate: u32,
    channels: u16,
}

impl Recording {
    /// Starts one writer per `(track, path)` and builds the set for the audio thread.
    pub(crate) fn begin(
        takes: &[(TrackId, PathBuf)],
        sample_rate: u32,
        channels: u16,
        latency_frames: u32,
    ) -> Result<(Self, RecordSet), RecordError> {
        if takes.is_empty() {
            return Err(RecordError::NoTakes);
        }
        let channels = channels.clamp(1, 2);
        let ring_frames = (sample_rate.max(1) * RING_SECONDS) as usize;
        let mut writers = Vec::with_capacity(takes.len());
        let mut producers = Vec::with_capacity(takes.len());
        for (track, path) in takes {
            let (writer, producer) = RecordWriter::start(path, channels, sample_rate, ring_frames)
                .map_err(|error| RecordError::Io(format!("{}: {error}", path.display())))?;
            writers.push(TakeWriter {
                track: *track,
                path: path.clone(),
                writer,
            });
            producers.push(producer);
        }
        let (set, shared) = RecordSet::new(producers, channels, latency_frames);
        Ok((
            Self {
                shared,
                writers,
                sample_rate,
                channels,
            },
            set,
        ))
    }

    /// How the audio thread's tap is doing (frames captured, where the takes start).
    pub fn shared(&self) -> &Arc<RecordShared> {
        &self.shared
    }

    /// Finishes every take file. Call only once the audio thread will push no more frames.
    pub(crate) fn finish(self) -> Result<Vec<FinishedTake>, RecordError> {
        let start_sample = if self.shared.started() {
            self.shared.start_sample()
        } else {
            0
        };
        let mut takes = Vec::with_capacity(self.writers.len());
        let mut failure = None;
        for take in self.writers {
            match take.writer.finish() {
                Ok(stats) => takes.push(FinishedTake {
                    track: take.track,
                    path: take.path,
                    frames: stats.frames,
                    dropped_frames: stats.dropped_frames,
                    start_sample,
                    sample_rate: self.sample_rate,
                    channels: self.channels,
                }),
                Err(error) => {
                    failure.get_or_insert_with(|| {
                        RecordError::Io(format!("{}: {error}", take.path.display()))
                    });
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(takes),
        }
    }
}

/// One finished take file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedTake {
    /// The track the take was recorded for.
    pub track: TrackId,
    /// The WAV file (32-bit float, `channels` channels at `sample_rate`).
    pub path: PathBuf,
    /// Frames in the file.
    pub frames: u64,
    /// Frames the take's ring had no room for (the writer fell behind); 0 when healthy.
    pub dropped_frames: u64,
    /// Where the take starts on the timeline, in samples at `sample_rate`: recording start minus
    /// the round-trip latency, at least 0.
    pub start_sample: u64,
    /// Sample rate of the file (the audio stream's).
    pub sample_rate: u32,
    /// Channels in the file.
    pub channels: u16,
}

/// What [`Engine::stop_recording`](crate::Engine::stop_recording) returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    /// The take files, in the order they were requested.
    pub takes: Vec<FinishedTake>,
    /// False when the audio thread did not confirm the stop in time, so the end of the takes
    /// may be missing.
    pub complete: bool,
}
