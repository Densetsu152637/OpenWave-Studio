//! Disk streaming with prefetch.
//!
//! A [`StreamSource`] owns a disk thread that reads the 32-bit float WAV in blocks and pushes
//! them through a lock-free ring ([`rtrb`]) ahead of the playhead. A second ring carries the
//! empty blocks back, so neither side allocates after [`StreamSource::open`]. The audio-thread
//! side only pops, copies and pushes preallocated blocks.
//!
//! # Seeking
//!
//! One atomic word holds a generation (24 bits) and a target frame (40 bits). A read that does
//! not continue where the last one ended (a seek, or a loop wrap) bumps the generation and sets
//! the target; the disk thread notices, seeks and starts a new run of blocks tagged with the new
//! generation, and the audio side discards blocks of older generations. Until the first block
//! of the new run arrives the read is an underrun (silence, counter incremented). The control
//! thread can avoid that with [`StreamHandle::seek_hint`] before playback reaches the spot.
//!
//! A read past the end of the file is silence and not an underrun.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::import::ImportError;
use crate::source::AudioSource;

/// Buffer sizes of a stream.
#[derive(Debug, Clone, Copy)]
pub struct StreamOptions {
    /// Frames per block.
    pub block_frames: usize,
    /// Blocks in flight (prefetch depth is `block_frames * blocks` frames).
    pub blocks: usize,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            block_frames: 2048,
            blocks: 64,
        }
    }
}

const POS_BITS: u32 = 40;
const POS_MASK: u64 = (1 << POS_BITS) - 1;
const GEN_MASK: u64 = (1 << 24) - 1;

fn pack(generation: u64, pos: u64) -> u64 {
    ((generation & GEN_MASK) << POS_BITS) | (pos & POS_MASK)
}

struct Shared {
    request: AtomicU64,
    prefetched_to: AtomicU64,
    /// Generation the disk thread has adopted (matches `request` once it caught up).
    disk_generation: AtomicU64,
    underruns: AtomicU64,
    stop: AtomicBool,
    pause: AtomicBool,
}

/// Control-thread view of a stream (cloneable, shareable).
#[derive(Clone)]
pub struct StreamHandle(Arc<Shared>);

impl StreamHandle {
    /// Reads that were (partly) silent because the disk thread had not delivered in time.
    pub fn underruns(&self) -> u64 {
        self.0.underruns.load(Ordering::Relaxed)
    }

    /// Asks the disk thread to start filling from `frame` now, so a coming seek finds data.
    pub fn seek_hint(&self, frame: u64) {
        let _ = self
            .0
            .request
            .try_update(Ordering::AcqRel, Ordering::Acquire, |r| {
                Some(pack((r >> POS_BITS) + 1, frame))
            });
    }

    /// The frame up to which the current run has been read from disk; 0 while the disk thread
    /// has not yet picked up the latest seek.
    pub fn prefetched_to(&self) -> u64 {
        let wanted = self.0.request.load(Ordering::Acquire) >> POS_BITS;
        if self.0.disk_generation.load(Ordering::Acquire) != wanted {
            return 0;
        }
        self.0.prefetched_to.load(Ordering::Acquire)
    }

    /// Pauses or resumes the disk thread (diagnostics and tests).
    pub fn pause_prefetch(&self, paused: bool) {
        self.0.pause.store(paused, Ordering::Release);
    }
}

struct Block {
    generation: u64,
    start: u64,
    frames: usize,
    data: Vec<f32>,
}

/// An [`AudioSource`] streaming a bundle WAV from disk.
pub struct StreamSource {
    channels: usize,
    frames: u64,
    shared: Arc<Shared>,
    filled: Consumer<Box<Block>>,
    free: Producer<Box<Block>>,
    current: Option<Box<Block>>,
    generation: u64,
    next_pos: u64,
    thread: Option<JoinHandle<()>>,
}

impl StreamSource {
    /// Opens `path` (32-bit float WAV) and starts prefetching from frame 0.
    pub fn open(path: &Path, options: StreamOptions) -> Result<Self, ImportError> {
        let bad = |m: &str| ImportError::Decode(m.to_string());
        let mut reader = hound::WavReader::new(BufReader::new(File::open(path)?))
            .map_err(|e| ImportError::Decode(e.to_string()))?;
        let spec = reader.spec();
        if spec.sample_format != hound::SampleFormat::Float || spec.bits_per_sample != 32 {
            return Err(bad("media is not 32-bit float WAV"));
        }
        let channels = usize::from(spec.channels);
        let frames = u64::from(reader.duration());
        if channels == 0 || frames > POS_MASK {
            return Err(bad("unsupported media"));
        }
        let blocks = options.blocks.max(2);
        let block_frames = options.block_frames.max(64);
        let (mut free_tx, mut free_rx) = RingBuffer::<Box<Block>>::new(blocks);
        let (filled_tx, filled_rx) = RingBuffer::<Box<Block>>::new(blocks);
        for _ in 0..blocks {
            let block = Box::new(Block {
                generation: 0,
                start: 0,
                frames: 0,
                data: vec![0.0; block_frames * channels],
            });
            let _ = free_tx.push(block);
        }
        let shared = Arc::new(Shared {
            request: AtomicU64::new(pack(0, 0)),
            prefetched_to: AtomicU64::new(0),
            disk_generation: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            pause: AtomicBool::new(false),
        });
        let disk = Arc::clone(&shared);
        let mut filled_tx = filled_tx;
        let thread = thread::Builder::new()
            .name("ow-media-stream".into())
            .spawn(move || {
                let mut generation = 0_u64;
                let mut pos = 0_u64;
                while !disk.stop.load(Ordering::Acquire) {
                    let request = disk.request.load(Ordering::Acquire);
                    if request >> POS_BITS != generation {
                        generation = request >> POS_BITS;
                        pos = request & POS_MASK;
                        if pos < frames && reader.seek(pos as u32).is_err() {
                            pos = frames;
                        }
                        disk.prefetched_to.store(pos, Ordering::Release);
                        disk.disk_generation.store(generation, Ordering::Release);
                    }
                    if disk.pause.load(Ordering::Acquire) || pos >= frames {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    let Ok(mut block) = free_rx.pop() else {
                        // No empty block: the audio side has not consumed yet.
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    };
                    let want = (frames - pos).min(block_frames as u64) as usize;
                    let mut got = 0;
                    let mut samples = reader.samples::<f32>();
                    for slot in block.data[..want * channels].iter_mut() {
                        match samples.next() {
                            Some(Ok(s)) => *slot = s,
                            _ => break,
                        }
                        got += 1;
                    }
                    let got_frames = got / channels;
                    block.data[got_frames * channels..want * channels].fill(0.0);
                    // A short read means a truncated file: pad the rest of the block.
                    block.generation = generation;
                    block.start = pos;
                    block.frames = want;
                    pos += want as u64;
                    // The ring holds as many slots as blocks exist, so this cannot fail.
                    let _ = filled_tx.push(block);
                    // Published after the push: `prefetched_to` promises the block is readable.
                    disk.prefetched_to.store(pos, Ordering::Release);
                }
            })
            .map_err(ImportError::Io)?;
        Ok(Self {
            channels,
            frames,
            shared,
            filled: filled_rx,
            free: free_tx,
            current: None,
            generation: 0,
            next_pos: 0,
            thread: Some(thread),
        })
    }

    /// The control-thread handle.
    pub fn handle(&self) -> StreamHandle {
        StreamHandle(Arc::clone(&self.shared))
    }

    fn recycle(&mut self, block: Box<Block>) {
        let _ = self.free.push(block);
    }
}

impl AudioSource for StreamSource {
    fn channels(&self) -> usize {
        self.channels
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn read(&mut self, position: u64, out: &mut [Vec<f32>], frames: usize) {
        // Adopt a seek the control thread requested, or request one for a discontinuity.
        let request = self.shared.request.load(Ordering::Acquire);
        if request >> POS_BITS != self.generation {
            self.generation = request >> POS_BITS;
            self.next_pos = request & POS_MASK;
            if let Some(block) = self.current.take() {
                self.recycle(block);
            }
        }
        if position != self.next_pos {
            let previous = self
                .shared
                .request
                .try_update(Ordering::AcqRel, Ordering::Acquire, |r| {
                    Some(pack((r >> POS_BITS) + 1, position))
                })
                .unwrap_or_else(|unchanged| unchanged);
            self.generation = ((previous >> POS_BITS) + 1) & GEN_MASK;
            if let Some(block) = self.current.take() {
                self.recycle(block);
            }
        }
        self.next_pos = position + frames as u64;

        let mut done = 0;
        let mut starved = false;
        while done < frames {
            let at = position + done as u64;
            if at >= self.frames {
                break;
            }
            // Find a block of the current run that is not entirely behind `at`.
            let usable = loop {
                match &self.current {
                    Some(b)
                        if b.generation == self.generation && at < b.start + b.frames as u64 =>
                    {
                        break true;
                    }
                    Some(_) => {
                        if let Some(old) = self.current.take() {
                            self.recycle(old);
                        }
                    }
                    None => match self.filled.pop() {
                        Ok(block) => self.current = Some(block),
                        Err(_) => break false,
                    },
                }
            };
            if !usable {
                starved = true;
                break;
            }
            let Some(block) = self.current.as_ref() else {
                break;
            };
            if at < block.start {
                // Frames before the block never arrived.
                let gap = ((block.start - at) as usize).min(frames - done);
                for channel in out.iter_mut() {
                    channel[done..done + gap].fill(0.0);
                }
                starved = true;
                done += gap;
                continue;
            }
            let from = (at - block.start) as usize;
            let n = (block.frames - from).min(frames - done);
            let channels = self.channels;
            for (c, channel) in out.iter_mut().enumerate().take(channels) {
                for (i, dst) in channel[done..done + n].iter_mut().enumerate() {
                    *dst = block.data[(from + i) * channels + c];
                }
            }
            done += n;
        }
        for channel in out.iter_mut() {
            channel[done..frames].fill(0.0);
        }
        if starved {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Drop for StreamSource {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
