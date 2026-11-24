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
    skipped: AtomicU64,
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

    /// Frames of recording-time input the tap could not use (a device buffer whose size did not
    /// match, or an input with too few channels); lost from every take.
    pub fn skipped(&self) -> u64 {
        self.skipped.load(Ordering::Acquire)
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

    /// Counts `frames` of input that could not be recorded this cycle.
    pub(crate) fn skip_frames(&self, frames: usize) {
        self.shared
            .skipped
            .fetch_add(frames as u64, Ordering::Release);
    }

    /// Pushes this cycle's interleaved `input` (`in_channels` channels per frame) to every take.
    /// `position` is the timeline position at the start of the cycle. Audio thread; allocation
    /// free.
    pub(crate) fn capture(&mut self, position: u64, input: &[f32], in_channels: usize) {
        if self.ended {
            return;
        }
        if in_channels < self.channels {
            self.skip_frames(input.len() / in_channels.max(1));
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

    /// Finishes every take file. Call only once the audio thread will push no more frames. A
    /// take whose writer failed is reported in [`Stopped::failures`]; the others are kept.
    pub(crate) fn finish(self) -> (Vec<FinishedTake>, Vec<TakeFailure>) {
        let start_sample = if self.shared.started() {
            self.shared.start_sample()
        } else {
            0
        };
        let skipped = self.shared.skipped();
        let results = self
            .writers
            .into_iter()
            .map(|take| (take.track, take.path, take.writer.finish()))
            .collect();
        collect_takes(
            results,
            start_sample,
            skipped,
            self.sample_rate,
            self.channels,
        )
    }
}

/// Splits writer results into finished takes and failures. `skipped` frames of input the tap
/// could not use are counted as dropped in every take.
fn collect_takes(
    results: Vec<(
        TrackId,
        PathBuf,
        std::io::Result<ow_media::record::RecordStats>,
    )>,
    start_sample: u64,
    skipped: u64,
    sample_rate: u32,
    channels: u16,
) -> (Vec<FinishedTake>, Vec<TakeFailure>) {
    let mut takes = Vec::with_capacity(results.len());
    let mut failures = Vec::new();
    for (track, path, result) in results {
        match result {
            Ok(stats) => takes.push(FinishedTake {
                track,
                path,
                frames: stats.frames,
                dropped_frames: stats.dropped_frames + skipped,
                start_sample,
                sample_rate,
                channels,
            }),
            Err(error) => failures.push(TakeFailure {
                track,
                path,
                error: error.to_string(),
            }),
        }
    }
    (takes, failures)
}

/// A take whose file could not be finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeFailure {
    /// The track the take was for.
    pub track: TrackId,
    /// The take file (it may hold part of the recording).
    pub path: PathBuf,
    /// What went wrong.
    pub error: String,
}

#[cfg(test)]
mod tests {
    use std::io;

    use ow_media::record::RecordStats;

    use super::*;

    #[test]
    fn input_the_tap_cannot_use_is_counted_as_skipped() {
        use crate::{CoreBuilder, Message, TransportCommand};
        let dir = tempfile::tempdir().unwrap();
        let (writer, producer) =
            RecordWriter::start(&dir.path().join("t.wav"), 2, 48_000, 48_000).unwrap();
        let (set, shared) = RecordSet::new(vec![producer], 2, 0);
        let (mut link, mut core) = CoreBuilder::new().workers(0).build();
        link.swap_record(set).unwrap();
        assert!(link.send(Message::Transport(TransportCommand::SetRecordArm(true))));
        assert!(link.send(Message::Transport(TransportCommand::Play)));
        let mut out = vec![0.0_f32; 64 * 2];
        // 64 output frames but an input that is not a whole number of frames per output frame.
        core.process(&mut out, 2, Some(&[0.5; 100]));
        assert_eq!(shared.skipped(), 64);
        assert_eq!(shared.captured(), 0);
        // A matching buffer records again.
        core.process(&mut out, 2, Some(&[0.5; 128]));
        assert_eq!(shared.captured(), 64);
        assert!(link.send(Message::EndRecording));
        core.process(&mut out, 2, Some(&[0.5; 128]));
        link.collect_retired();
        assert_eq!(writer.finish().unwrap().frames, 64);
    }

    #[test]
    fn one_failing_writer_does_not_lose_the_other_takes() {
        let ok = |frames| {
            Ok(RecordStats {
                frames,
                dropped_frames: 0,
            })
        };
        let results = vec![
            (TrackId(1), PathBuf::from("a.wav"), ok(100)),
            (
                TrackId(2),
                PathBuf::from("b.wav"),
                Err(io::Error::other("disk full")),
            ),
            (TrackId(3), PathBuf::from("c.wav"), ok(100)),
        ];
        let (takes, failures) = collect_takes(results, 10, 7, 48_000, 2);
        assert_eq!(
            takes.iter().map(|t| t.track).collect::<Vec<_>>(),
            [TrackId(1), TrackId(3)]
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].track, TrackId(2));
        assert!(failures[0].error.contains("disk full"));
        // Input the tap skipped counts as lost in every take.
        assert!(
            takes
                .iter()
                .all(|t| t.dropped_frames == 7 && t.start_sample == 10)
        );
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
    /// False when the audio thread did not confirm the stop in time, or the tap skipped input
    /// ([`RecordShared::skipped`]), so parts of the takes may be missing.
    pub complete: bool,
    /// Takes whose files could not be finished; the others are in `takes`.
    pub failures: Vec<TakeFailure>,
}
