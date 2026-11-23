//! The recording writer: a thread that drains a lock-free ring into a 32-bit float WAV with
//! hound. The audio thread owns the [`RecordProducer`] and only copies samples into the ring;
//! file creation, writing and finalizing happen on the writer thread. Stage 14 wires it to
//! the engine's inputs.

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rtrb::{Producer, RingBuffer};

struct Shared {
    stop: AtomicBool,
    dropped: AtomicU64,
}

/// What a finished recording holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordStats {
    /// Frames written to the file.
    pub frames: u64,
    /// Frames the ring had no room for (the writer fell behind); 0 for a healthy recording.
    pub dropped_frames: u64,
}

/// Audio-thread end: pushes interleaved samples, never blocks.
pub struct RecordProducer {
    ring: Producer<f32>,
    channels: usize,
    shared: Arc<Shared>,
}

impl RecordProducer {
    /// Pushes whole frames from `interleaved` (a multiple of the channel count). Frames that
    /// do not fit are dropped and counted; returns the frames accepted.
    pub fn push(&mut self, interleaved: &[f32]) -> usize {
        let frames = interleaved.len() / self.channels;
        let fit = self.ring.slots() / self.channels;
        let take = frames.min(fit);
        if take < frames {
            self.shared
                .dropped
                .fetch_add((frames - take) as u64, Ordering::Relaxed);
        }
        if take > 0
            && let Ok(chunk) = self.ring.write_chunk_uninit(take * self.channels)
        {
            chunk.fill_from_iter(interleaved[..take * self.channels].iter().copied());
        }
        take
    }
}

/// Control-thread end: owns the writer thread.
pub struct RecordWriter {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<io::Result<u64>>>,
}

impl RecordWriter {
    /// Creates `path` and starts the writer; `ring_frames` is the ring capacity (about two
    /// seconds is generous).
    pub fn start(
        path: &Path,
        channels: u16,
        sample_rate: u32,
        ring_frames: usize,
    ) -> io::Result<(Self, RecordProducer)> {
        let channels = usize::from(channels.max(1));
        let spec = crate::import::wav_spec(channels, sample_rate);
        let mut writer = hound::WavWriter::new(BufWriter::new(File::create(path)?), spec)
            .map_err(io::Error::other)?;
        let (producer, mut consumer) = RingBuffer::<f32>::new(ring_frames.max(64) * channels);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
        });
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("ow-media-record".into())
            .spawn(move || {
                let mut samples = 0_u64;
                loop {
                    // Read the stop flag first: whatever was pushed before it is drained below.
                    let stopping = thread_shared.stop.load(Ordering::Acquire);
                    let available = consumer.slots();
                    if available > 0 {
                        if let Ok(chunk) = consumer.read_chunk(available) {
                            let (a, b) = chunk.as_slices();
                            for s in a.iter().chain(b) {
                                writer.write_sample(*s).map_err(io::Error::other)?;
                            }
                            samples += available as u64;
                            chunk.commit_all();
                        }
                    } else if stopping {
                        break;
                    } else {
                        thread::sleep(Duration::from_millis(2));
                    }
                }
                writer.finalize().map_err(io::Error::other)?;
                Ok(samples / channels as u64)
            })?;
        Ok((
            Self {
                shared: Arc::clone(&shared),
                thread: Some(thread),
            },
            RecordProducer {
                ring: producer,
                channels,
                shared,
            },
        ))
    }

    /// Drains what was pushed, finalizes the file and reports totals. Push everything before
    /// calling this.
    pub fn finish(mut self) -> io::Result<RecordStats> {
        self.shared.stop.store(true, Ordering::Release);
        let frames = match self.thread.take() {
            Some(thread) => thread
                .join()
                .map_err(|_| io::Error::other("writer thread panicked"))??,
            None => 0,
        };
        Ok(RecordStats {
            frames,
            dropped_frames: self.shared.dropped.load(Ordering::Relaxed),
        })
    }
}

impl Drop for RecordWriter {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
