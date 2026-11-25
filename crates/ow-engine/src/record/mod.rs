//! Audio recording: the record tap that runs on the audio thread and the control-thread
//! [`Recording`] that owns the take files.
//!
//! A recording is one [`RecordSet`]: per armed track a [`RecordTrack`] with the input channels it
//! records and one [`RecordProducer`] per pass (one, or up to [`MAX_LOOP_TAKES`] when the
//! transport loops), handed to the audio thread through its own wait-free ring
//! ([`ControlLink::swap_record`](crate::ControlLink)) and handed back the same way
//! ([`ControlLink::collect_retired`](crate::ControlLink)), so the audio thread never frees a
//! ring. While recording, [`AudioCore::process`] pushes each track's routed input channels into
//! the current pass's producer with no allocation, lock or I/O; a writer thread per take
//! (`ow-media`) turns the rings into WAV files.
//!
//! What is recorded is decided per frame from the transport's own block timing
//! ([`Span::record_frames`](crate::Span::record_frames)): frames of a count-in are never
//! recorded, with a punch range only the frames inside it are, and each wrap of the loop starts
//! the next pass at exactly the wrap frame (a pass is one take; passes beyond
//! [`MAX_LOOP_TAKES`] are dropped and counted, see [`RecordShared::dropped_passes`]).
//!
//! Placement. A sound the engine plays is heard back on the input one round trip
//! (`latency_frames`, input plus output) later, so input frame `i` of a pass whose first
//! recorded frame was at timeline position `P` belongs to position `P + i - latency`. A take
//! therefore starts at `P - latency`; when that is before the timeline start, the take's first
//! `latency - P` frames (which belong to negative positions) are dropped and it starts at 0.
//! `P` is the recording start (or the punch-in, or the loop start for later passes).
//!
//! Monitoring is not part of the tap: a track's input reaches its strip through
//! [`InputMonitorNode`](crate::graph::input::InputMonitorNode), compiled when the track's
//! monitor mode asks for it.
//!
//! Stopping: [`Engine::stop_recording`](crate::Engine::stop_recording) stops the transport and
//! sends `EndRecording`; the audio thread marks the set finished (it never pushes again) and
//! retires it, and only then are the writers finished, so every frame the tap pushed is on disk
//! (each take's length equals the frames recorded in its pass, see
//! [`RecordShared::pass_captured`]).
//!
//! Not recorded: MIDI (stage 15), crossfades between takes, region comping.
//!
//! [`AudioCore::process`]: crate::AudioCore::process

use std::fmt;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use ow_media::{RecordProducer, RecordWriter};
use ow_model::TrackId;

use crate::graph::MAX_BLOCK;
use crate::transport::{BlockTiming, Span};

/// Seconds of audio each take's ring holds before the writer thread must have drained it.
pub const RING_SECONDS: u32 = 2;

/// Passes (takes per track) one loop recording keeps; later passes are dropped.
pub const MAX_LOOP_TAKES: usize = 16;

/// What the tap and the control side both see; the audio thread only does atomic stores here.
#[derive(Debug, Default)]
pub struct RecordShared {
    started: AtomicBool,
    /// Passes begun (the highest pass index recorded plus one).
    passes: AtomicU32,
    pass_start: [AtomicU64; MAX_LOOP_TAKES],
    pass_captured: [AtomicU64; MAX_LOOP_TAKES],
    skipped: AtomicU64,
    dropped_passes: AtomicU32,
    done: AtomicBool,
}

impl RecordShared {
    /// True once the tap has captured its first frame.
    pub fn started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    /// Passes the tap has begun recording: 1 without a loop (once started), one more for every
    /// loop wrap that recorded something.
    pub fn passes(&self) -> usize {
        self.passes.load(Ordering::Acquire) as usize
    }

    /// Timeline sample at which the takes of pass `pass` start (the pass's first recorded
    /// position minus the round-trip latency, at least 0); meaningful for `pass < passes()`.
    pub fn pass_start(&self, pass: usize) -> u64 {
        self.pass_start
            .get(pass)
            .map_or(0, |start| start.load(Ordering::Acquire))
    }

    /// Frames the tap pushed to each take's ring in pass `pass`, after dropping the leading
    /// frames that belong before the timeline start. Each take file holds exactly this many
    /// frames once finished, less whatever its ring had no room for
    /// ([`FinishedTake::dropped_frames`]).
    pub fn pass_captured(&self, pass: usize) -> u64 {
        self.pass_captured
            .get(pass)
            .map_or(0, |frames| frames.load(Ordering::Acquire))
    }

    /// Timeline sample at which the first pass's takes start; meaningful once
    /// [`started`](Self::started).
    pub fn start_sample(&self) -> u64 {
        self.pass_start(0)
    }

    /// Frames pushed to each take in all passes together (for one pass, that pass's frames).
    pub fn captured(&self) -> u64 {
        (0..MAX_LOOP_TAKES)
            .map(|pass| self.pass_captured(pass))
            .sum()
    }

    /// Frames of recording-time input the tap could not use (a device buffer whose size did not
    /// match, or an input with too few channels); lost from every take.
    pub fn skipped(&self) -> u64 {
        self.skipped.load(Ordering::Acquire)
    }

    /// Loop passes beyond [`MAX_LOOP_TAKES`] (or beyond the takes that were prepared) that were
    /// not recorded.
    pub fn dropped_passes(&self) -> u32 {
        self.dropped_passes.load(Ordering::Acquire)
    }

    /// True once the audio thread will push no more frames for this recording.
    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

/// One armed track on the audio thread: the input channels it records and a producer per pass.
pub struct RecordTrack {
    first: usize,
    channels: usize,
    producers: Vec<RecordProducer>,
}

impl RecordTrack {
    /// A track recording `channels` (1 or 2) input channels from `first_channel`, one producer
    /// per pass; every producer was created with `channels` channels.
    pub fn new(first_channel: u16, channels: u16, passes: Vec<RecordProducer>) -> Self {
        Self {
            first: usize::from(first_channel),
            channels: usize::from(channels.clamp(1, 2)),
            producers: passes,
        }
    }
}

/// The audio-thread half of a recording: the tracks plus the placement rules.
pub struct RecordSet {
    tracks: Vec<RecordTrack>,
    /// Passes every track has a producer for.
    max_passes: usize,
    latency: u64,
    /// The pass being recorded.
    pass: usize,
    /// Whether the current pass has recorded its first frame.
    pass_started: bool,
    /// Leading frames of the current pass still to drop (those that belong before the
    /// timeline start).
    skip: u64,
    /// Timeline sample the next playing frame is expected at; a different one is a loop wrap
    /// (or a locate).
    expected: Option<u64>,
    scratch: Vec<f32>,
    ended: bool,
    shared: Arc<RecordShared>,
}

impl RecordSet {
    /// Builds the set for `tracks` and the round-trip `latency_frames`. The shared state is how
    /// the control side watches the recording.
    pub fn new(tracks: Vec<RecordTrack>, latency_frames: u32) -> (Self, Arc<RecordShared>) {
        let shared = Arc::new(RecordShared::default());
        let max_passes = tracks
            .iter()
            .map(|track| track.producers.len())
            .min()
            .unwrap_or(0)
            .min(MAX_LOOP_TAKES);
        let set = Self {
            tracks,
            max_passes,
            latency: u64::from(latency_frames),
            pass: 0,
            pass_started: false,
            skip: 0,
            expected: None,
            scratch: vec![0.0; MAX_BLOCK * 2],
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

    /// Records one block. `timing` is the block's transport timing and `input` its interleaved
    /// input with the channel count per frame (0 when the buffer did not hold a whole number of
    /// channels; `None` when the device has no input). Audio thread; allocation free.
    pub(crate) fn tap(&mut self, timing: BlockTiming<'_>, input: Option<(&[f32], usize)>) {
        if self.ended {
            return;
        }
        let looped = timing.loop_range().is_some();
        for span in timing.spans() {
            let start = span.start_sample();
            if looped && self.pass_started && self.expected.is_some_and(|next| next != start) {
                self.pass += 1;
                self.pass_started = false;
            }
            self.expected = Some(start + span.frames() as u64);
            if let Some(range) = span.record_frames() {
                self.record(&span, range, input);
            }
        }
    }

    /// Records the block frames `range` of `span` into the current pass.
    fn record(&mut self, span: &Span<'_>, range: Range<usize>, input: Option<(&[f32], usize)>) {
        let Some((input, in_channels)) = input else {
            return;
        };
        if in_channels == 0 || input.len() < range.end * in_channels {
            self.skip_frames(range.len());
            return;
        }
        if self.pass >= self.max_passes {
            // No take for this pass: count it once, and let the next wrap move on.
            if !self.pass_started {
                self.pass_started = true;
                self.shared.dropped_passes.fetch_add(1, Ordering::Release);
            }
            return;
        }
        if !self.pass_started {
            let first = span.sample_at(range.start);
            self.skip = self.latency.saturating_sub(first);
            self.shared.pass_start[self.pass]
                .store(first.saturating_sub(self.latency), Ordering::Release);
            self.shared
                .passes
                .store(self.pass as u32 + 1, Ordering::Release);
            self.shared.started.store(true, Ordering::Release);
            self.pass_started = true;
        }
        let dropped = self.skip.min(range.len() as u64) as usize;
        self.skip -= dropped as u64;
        let mut from = range.start + dropped;
        while from < range.end {
            let count = (range.end - from).min(MAX_BLOCK);
            let rows = &input[from * in_channels..(from + count) * in_channels];
            let mut usable = true;
            for track in &mut self.tracks {
                usable &= track.push(self.pass, rows, in_channels, &mut self.scratch);
            }
            if !usable {
                self.skip_frames(count);
            }
            self.shared.pass_captured[self.pass].fetch_add(count as u64, Ordering::Release);
            from += count;
        }
    }
}

impl RecordTrack {
    /// Pushes `rows` (interleaved, `in_channels` per frame) to the pass's producer; false when
    /// the input has too few channels for this track.
    fn push(&mut self, pass: usize, rows: &[f32], in_channels: usize, scratch: &mut [f32]) -> bool {
        if self.first + self.channels > in_channels {
            return false;
        }
        let Some(producer) = self.producers.get_mut(pass) else {
            return true;
        };
        if in_channels == self.channels {
            producer.push(rows);
        } else {
            let frames = rows.len() / in_channels;
            for (frame, out) in rows
                .chunks_exact(in_channels)
                .zip(scratch.chunks_exact_mut(self.channels))
            {
                out.copy_from_slice(&frame[self.first..self.first + self.channels]);
            }
            producer.push(&scratch[..frames * self.channels]);
        }
        true
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
    /// A track's input channels are not on the open input device.
    BadInput {
        /// The track.
        track: TrackId,
        /// Channels the open input device has.
        device_channels: u16,
    },
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
            Self::BadInput {
                track,
                device_channels,
            } => write!(
                f,
                "track {} records from input channels the input device does not have \
                 (it has {device_channels})",
                track.0
            ),
            Self::Io(message) => write!(f, "could not write the recording: {message}"),
        }
    }
}

impl std::error::Error for RecordError {}

/// What to record for one armed track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackTakes {
    /// The track.
    pub track: TrackId,
    /// The input channels: `(first channel, 1 or 2 channels)`; `None` records channel 0 on a
    /// mono device and channels 0 and 1 on a wider one.
    pub input: Option<(u16, u16)>,
    /// One WAV path per pass: a single path without a loop, up to [`MAX_LOOP_TAKES`] with one.
    /// A pass without a path is not recorded.
    pub paths: Vec<PathBuf>,
}

/// What [`Engine::start_recording_plan`](crate::Engine::start_recording_plan) sets up besides
/// the takes: the transport's loop, punch and count-in for the recording. They are cleared again
/// when the recording stops.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordPlan {
    /// The armed tracks and their take files.
    pub tracks: Vec<TrackTakes>,
    /// The loop range in beats; each pass becomes a take (give each track up to
    /// [`MAX_LOOP_TAKES`] paths).
    pub loop_beats: Option<(f64, f64)>,
    /// The punch range in beats: only frames inside it are recorded.
    pub punch_beats: Option<(f64, f64)>,
    /// Bars of count-in (0 for none); nothing is recorded during it.
    pub count_in_bars: u32,
}

impl RecordPlan {
    /// One take per `(track, path)` on the default input, no loop, punch or count-in.
    pub fn simple(takes: &[(TrackId, PathBuf)]) -> Self {
        Self {
            tracks: takes
                .iter()
                .map(|(track, path)| TrackTakes {
                    track: *track,
                    input: None,
                    paths: vec![path.clone()],
                })
                .collect(),
            loop_beats: None,
            punch_beats: None,
            count_in_bars: 0,
        }
    }
}

struct TakeWriter {
    track: TrackId,
    pass: u32,
    channels: u16,
    path: PathBuf,
    writer: RecordWriter,
}

/// A running recording (control thread): the take files being written, one per armed track and
/// pass. Give it back to [`Engine::stop_recording`](crate::Engine::stop_recording) to finish it.
pub struct Recording {
    pub(crate) shared: Arc<RecordShared>,
    writers: Vec<TakeWriter>,
    sample_rate: u32,
}

impl Recording {
    /// Starts one writer per track and pass and builds the set for the audio thread. The device
    /// has `input_channels` input channels.
    pub(crate) fn begin(
        tracks: &[TrackTakes],
        sample_rate: u32,
        input_channels: u16,
        latency_frames: u32,
    ) -> Result<(Self, RecordSet), RecordError> {
        if tracks.is_empty() || tracks.iter().any(|t| t.paths.is_empty()) {
            return Err(RecordError::NoTakes);
        }
        let ring_frames = (sample_rate.max(1) * RING_SECONDS) as usize;
        let mut writers = Vec::new();
        let mut record_tracks = Vec::with_capacity(tracks.len());
        for take in tracks {
            let (first, channels) = take.input.unwrap_or((0, input_channels.clamp(1, 2)));
            let channels = channels.clamp(1, 2);
            if u32::from(first) + u32::from(channels) > u32::from(input_channels) {
                return Err(RecordError::BadInput {
                    track: take.track,
                    device_channels: input_channels,
                });
            }
            let mut producers = Vec::with_capacity(take.paths.len().min(MAX_LOOP_TAKES));
            for (pass, path) in take.paths.iter().take(MAX_LOOP_TAKES).enumerate() {
                let (writer, producer) =
                    RecordWriter::start(path, channels, sample_rate, ring_frames)
                        .map_err(|error| RecordError::Io(format!("{}: {error}", path.display())))?;
                writers.push(TakeWriter {
                    track: take.track,
                    pass: pass as u32,
                    channels,
                    path: path.clone(),
                    writer,
                });
                producers.push(producer);
            }
            record_tracks.push(RecordTrack::new(first, channels, producers));
        }
        let (set, shared) = RecordSet::new(record_tracks, latency_frames);
        Ok((
            Self {
                shared,
                writers,
                sample_rate,
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
        let passes = self.shared.passes();
        let starts: Vec<u64> = (0..MAX_LOOP_TAKES)
            .map(|pass| {
                if pass < passes {
                    self.shared.pass_start(pass)
                } else {
                    0
                }
            })
            .collect();
        let skipped = self.shared.skipped();
        let sample_rate = self.sample_rate;
        let results = self
            .writers
            .into_iter()
            .map(|take| {
                (
                    take.track,
                    take.pass,
                    take.channels,
                    take.path,
                    take.writer.finish(),
                )
            })
            .collect();
        collect_takes(results, &starts, skipped, sample_rate)
    }
}

/// One writer's outcome: track, pass, channels, file and the writer's result.
type WriterResult = (
    TrackId,
    u32,
    u16,
    PathBuf,
    std::io::Result<ow_media::record::RecordStats>,
);

/// Splits writer results into finished takes and failures. `starts[pass]` is where each pass's
/// takes start; `skipped` frames of input the tap could not use are counted as dropped in
/// every take.
fn collect_takes(
    results: Vec<WriterResult>,
    starts: &[u64],
    skipped: u64,
    sample_rate: u32,
) -> (Vec<FinishedTake>, Vec<TakeFailure>) {
    let mut takes = Vec::with_capacity(results.len());
    let mut failures = Vec::new();
    for (track, pass, channels, path, result) in results {
        match result {
            Ok(stats) => takes.push(FinishedTake {
                track,
                pass,
                path,
                frames: stats.frames,
                dropped_frames: stats.dropped_frames + skipped,
                start_sample: starts.get(pass as usize).copied().unwrap_or(0),
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

/// One finished take file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedTake {
    /// The track the take was recorded for.
    pub track: TrackId,
    /// The loop pass the take is from, 0 for the first (and for a recording without a loop).
    pub pass: u32,
    /// The WAV file (32-bit float, `channels` channels at `sample_rate`).
    pub path: PathBuf,
    /// Frames in the file; 0 for a pass that never recorded.
    pub frames: u64,
    /// Frames the take's ring had no room for (the writer fell behind); 0 when healthy.
    pub dropped_frames: u64,
    /// Where the take starts on the timeline, in samples at `sample_rate`: its pass's first
    /// recorded position minus the round-trip latency, at least 0.
    pub start_sample: u64,
    /// Sample rate of the file (the audio stream's).
    pub sample_rate: u32,
    /// Channels in the file.
    pub channels: u16,
}

/// What [`Engine::stop_recording`](crate::Engine::stop_recording) returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    /// The take files, by track in the order they were requested and then by pass.
    pub takes: Vec<FinishedTake>,
    /// False when the audio thread did not confirm the stop in time, or the tap skipped input
    /// ([`RecordShared::skipped`]), so parts of the takes may be missing.
    pub complete: bool,
    /// Takes whose files could not be finished; the others are in `takes`.
    pub failures: Vec<TakeFailure>,
    /// Loop passes that were not recorded because the takes ran out
    /// ([`RecordShared::dropped_passes`]).
    pub dropped_passes: u32,
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
        let (set, shared) = RecordSet::new(vec![RecordTrack::new(0, 2, vec![producer])], 0);
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
            (TrackId(1), 0, 2, PathBuf::from("a.wav"), ok(100)),
            (
                TrackId(2),
                0,
                2,
                PathBuf::from("b.wav"),
                Err(io::Error::other("disk full")),
            ),
            (TrackId(3), 1, 1, PathBuf::from("c.wav"), ok(100)),
        ];
        let mut starts = vec![0; MAX_LOOP_TAKES];
        starts[0] = 10;
        starts[1] = 30;
        let (takes, failures) = collect_takes(results, &starts, 7, 48_000);
        assert_eq!(
            takes.iter().map(|t| t.track).collect::<Vec<_>>(),
            [TrackId(1), TrackId(3)]
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].track, TrackId(2));
        assert!(failures[0].error.contains("disk full"));
        // Input the tap skipped counts as lost in every take; each take starts with its pass.
        assert!(takes.iter().all(|t| t.dropped_frames == 7));
        assert_eq!(
            takes.iter().map(|t| t.start_sample).collect::<Vec<_>>(),
            [10, 30]
        );
        assert_eq!(takes[1].channels, 1);
    }
}
