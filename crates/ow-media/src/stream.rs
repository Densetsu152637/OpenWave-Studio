//! Disk streaming with prefetch.
//!
//! A [`StreamSource`] owns a disk thread that reads the 32-bit float WAV in blocks and pushes
//! them through lock-free rings ([`rtrb`]) ahead of the playhead. A second ring per lane
//! carries the empty blocks back, so neither side allocates after [`StreamSource::open`]. The
//! audio-thread side only pops, copies and pushes preallocated blocks.
//!
//! # Lanes, seeks and hints
//!
//! There are two *lanes*, each with its own pair of rings and its own read position. One lane
//! is active (the one playback reads); the other is the spare. Each lane has one atomic word
//! holding a generation (24 bits) and a target frame (40 bits); the disk thread follows both
//! words and fills both lanes, tagging every block with the generation it was read for.
//!
//! * [`StreamHandle::seek_hint`] points the spare lane at a frame. The disk thread pre-fills it
//!   while the active lane keeps playing untouched, so contiguous playback is never disturbed.
//! * A read that does not continue the previous one switches lanes when its position equals the
//!   spare lane's hinted frame: the data is already waiting, with no underrun. This is how a
//!   gapless loop wrap works: hint the loop start while playing, and the wrap lands on it.
//! * Any other discontinuity re-aims the active lane (bumping its generation) and plays silence
//!   (one underrun) until the first block arrives.
//!
//! The control thread picks the spare lane from a relaxed read of the active lane. If a hint
//! races with a lane switch and lands on the lane just activated, the audio side notices the
//! generation change and re-seeks; samples are never wrong, but that block may be an underrun.
//!
//! A read past the end of the file is silence and not an underrun.
//!
//! Memory: each lane holds `blocks` blocks of `block_frames` frames, allocated once at open.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::import::ImportError;
use crate::source::AudioSource;
use crate::wav::wav_info;

/// Buffer sizes of a stream (per lane).
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
/// Target frame of a lane that has nothing to read.
const IDLE: u64 = POS_MASK;

fn pack(generation: u64, pos: u64) -> u64 {
    ((generation & GEN_MASK) << POS_BITS) | (pos & POS_MASK)
}

fn generation_of(word: u64) -> u64 {
    word >> POS_BITS
}

struct LaneShared {
    request: AtomicU64,
    prefetched_to: AtomicU64,
    /// Generation the disk thread has adopted (matches `request` once it caught up).
    disk_generation: AtomicU64,
}

impl LaneShared {
    fn new(request: u64) -> Self {
        Self {
            request: AtomicU64::new(request),
            prefetched_to: AtomicU64::new(0),
            disk_generation: AtomicU64::new(0),
        }
    }

    /// Re-aims the lane at `pos` under a new generation; returns that generation.
    fn aim(&self, pos: u64) -> u64 {
        let previous = self
            .request
            .try_update(Ordering::AcqRel, Ordering::Acquire, |r| {
                Some(pack(generation_of(r) + 1, pos))
            })
            .unwrap_or_else(|unchanged| unchanged);
        (generation_of(previous) + 1) & GEN_MASK
    }
}

struct Shared {
    lanes: [LaneShared; 2],
    /// The lane playback reads (written by the audio side).
    active: AtomicUsize,
    /// The lane most recently aimed, for [`StreamHandle::prefetched_to`].
    last_aimed: AtomicUsize,
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

    /// Asks the disk thread to start filling the spare lane from `frame`, while playback
    /// continues undisturbed. The next read at exactly `frame` that is not a continuation
    /// (a loop wrap, a locate) switches to that data without an underrun.
    pub fn seek_hint(&self, frame: u64) {
        let lane = 1 - self.0.active.load(Ordering::Acquire);
        self.0.lanes[lane].aim(frame.min(IDLE - 1));
        self.0.last_aimed.store(lane, Ordering::Release);
    }

    /// The frame up to which the most recently aimed lane has been read from disk; 0 while the
    /// disk thread has not yet picked up that aim.
    pub fn prefetched_to(&self) -> u64 {
        let lane = &self.0.lanes[self.0.last_aimed.load(Ordering::Acquire)];
        let wanted = generation_of(lane.request.load(Ordering::Acquire));
        if lane.disk_generation.load(Ordering::Acquire) != wanted {
            return 0;
        }
        lane.prefetched_to.load(Ordering::Acquire)
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

/// Audio-side ends of one lane's rings.
struct AudioLane {
    filled: Consumer<Box<Block>>,
    free: Producer<Box<Block>>,
}

impl AudioLane {
    fn recycle(&mut self, block: Box<Block>) {
        // The rings hold as many slots as blocks exist, so this cannot fail.
        let _ = self.free.push(block);
    }

    /// Returns every filled block that is not of `keep` to the disk thread.
    fn discard_stale(&mut self, keep: Option<u64>) {
        loop {
            let stale = match (self.filled.peek(), keep) {
                (Ok(block), Some(keep)) => block.generation != keep,
                (Ok(_), None) => true,
                (Err(_), _) => false,
            };
            if !stale {
                break;
            }
            match self.filled.pop() {
                Ok(block) => self.recycle(block),
                Err(_) => break,
            }
        }
    }
}

/// An [`AudioSource`] streaming a bundle WAV from disk.
pub struct StreamSource {
    channels: usize,
    frames: u64,
    shared: Arc<Shared>,
    lanes: [AudioLane; 2],
    active: usize,
    current: Option<Box<Block>>,
    generation: u64,
    next_pos: u64,
    thread: Option<JoinHandle<()>>,
}

/// Disk-thread state of one lane.
struct DiskLane {
    free: Consumer<Box<Block>>,
    filled: Producer<Box<Block>>,
    generation: u64,
    pos: u64,
}

impl StreamSource {
    /// Opens `path` (32-bit float WAV, at most 8 channels; the length is clamped to the bytes
    /// the file holds) and starts prefetching from frame 0.
    pub fn open(path: &Path, options: StreamOptions) -> Result<Self, ImportError> {
        let info = wav_info(path, true)?;
        let channels = usize::from(info.spec.channels);
        let frames = info.frames;
        if frames >= IDLE {
            return Err(ImportError::TooLarge("duration"));
        }
        let mut reader = hound::WavReader::new(BufReader::new(File::open(path)?))
            .map_err(|e| ImportError::Decode(e.to_string()))?;
        let blocks = options.blocks.max(2);
        let block_frames = options.block_frames.max(64);

        let mut audio = Vec::new();
        let mut disk = Vec::new();
        for _ in 0..2 {
            let (mut free_tx, free_rx) = RingBuffer::<Box<Block>>::new(blocks);
            let (filled_tx, filled_rx) = RingBuffer::<Box<Block>>::new(blocks);
            for _ in 0..blocks {
                let _ = free_tx.push(Box::new(Block {
                    generation: 0,
                    start: 0,
                    frames: 0,
                    data: vec![0.0; block_frames * channels],
                }));
            }
            audio.push(AudioLane {
                filled: filled_rx,
                free: free_tx,
            });
            disk.push(DiskLane {
                free: free_rx,
                filled: filled_tx,
                generation: 0,
                pos: 0,
            });
        }
        let shared = Arc::new(Shared {
            // Lane 0 starts at frame 0; lane 1 is idle.
            lanes: [LaneShared::new(pack(0, 0)), LaneShared::new(pack(0, IDLE))],
            active: AtomicUsize::new(0),
            last_aimed: AtomicUsize::new(0),
            underruns: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            pause: AtomicBool::new(false),
        });
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("ow-media-stream".into())
            .spawn(move || {
                let mut lanes = [disk.remove(0), disk.remove(0)];
                lanes[1].pos = frames; // the spare lane starts idle
                let mut reader_pos = 0_u64;
                while !thread_shared.stop.load(Ordering::Acquire) {
                    let mut worked = false;
                    for (index, lane) in lanes.iter_mut().enumerate() {
                        let shared = &thread_shared.lanes[index];
                        let request = shared.request.load(Ordering::Acquire);
                        if generation_of(request) != lane.generation {
                            lane.generation = generation_of(request);
                            lane.pos = (request & POS_MASK).min(frames);
                            shared.prefetched_to.store(lane.pos, Ordering::Release);
                            shared
                                .disk_generation
                                .store(lane.generation, Ordering::Release);
                        }
                        if thread_shared.pause.load(Ordering::Acquire) || lane.pos >= frames {
                            continue;
                        }
                        if lane.free.is_empty() {
                            continue; // the audio side has not consumed yet
                        }
                        if reader_pos != lane.pos {
                            if reader.seek(lane.pos as u32).is_err() {
                                lane.pos = frames;
                                continue;
                            }
                            reader_pos = lane.pos;
                        }
                        let Ok(mut block) = lane.free.pop() else {
                            continue;
                        };
                        let want = (frames - lane.pos).min(block_frames as u64) as usize;
                        let mut got = 0;
                        let mut samples = reader.samples::<f32>();
                        for slot in &mut block.data[..want * channels] {
                            match samples.next() {
                                Some(Ok(s)) => *slot = s,
                                _ => break,
                            }
                            got += 1;
                        }
                        // A short read means a truncated file: pad the rest of the block.
                        block.data[got..want * channels].fill(0.0);
                        block.generation = lane.generation;
                        block.start = lane.pos;
                        block.frames = want;
                        lane.pos += want as u64;
                        reader_pos = lane.pos;
                        // The ring holds as many slots as blocks exist, so this cannot fail.
                        let _ = lane.filled.push(block);
                        // Published after the push: `prefetched_to` promises the block is
                        // readable.
                        shared.prefetched_to.store(lane.pos, Ordering::Release);
                        worked = true;
                    }
                    if !worked {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            })
            .map_err(ImportError::Io)?;
        let [lane0, lane1] = <[AudioLane; 2]>::try_from(audio)
            .unwrap_or_else(|_| unreachable!("two lanes were built"));
        Ok(Self {
            channels,
            frames,
            shared,
            lanes: [lane0, lane1],
            active: 0,
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

    fn drop_current(&mut self) {
        if let Some(block) = self.current.take() {
            self.lanes[self.active].recycle(block);
        }
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
        let shared = Arc::clone(&self.shared);
        // A change to the active lane's aim that this side did not make (a hint that raced a
        // lane switch): follow it.
        let aim = shared.lanes[self.active].request.load(Ordering::Acquire);
        if generation_of(aim) != self.generation {
            self.drop_current();
            self.generation = generation_of(aim);
            self.next_pos = aim & POS_MASK;
        }
        if position != self.next_pos {
            let other = 1 - self.active;
            let hinted = shared.lanes[other].request.load(Ordering::Acquire);
            if hinted & POS_MASK == position && position < self.frames {
                // The spare lane has been filling from here: switch to it.
                self.drop_current();
                self.lanes[self.active].discard_stale(None);
                shared.lanes[self.active].aim(IDLE);
                self.active = other;
                self.generation = generation_of(hinted);
                shared.active.store(other, Ordering::Release);
            } else {
                self.drop_current();
                self.generation = shared.lanes[self.active].aim(position);
                shared.last_aimed.store(self.active, Ordering::Release);
            }
        }
        self.next_pos = position + frames as u64;
        // Blocks the spare lane read for an earlier hint are not needed once it is re-aimed.
        let spare = 1 - self.active;
        let spare_generation = generation_of(shared.lanes[spare].request.load(Ordering::Acquire));
        self.lanes[spare].discard_stale(Some(spare_generation));

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
                    Some(_) => self.drop_current(),
                    None => match self.lanes[self.active].filled.pop() {
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
            shared.underruns.fetch_add(1, Ordering::Relaxed);
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
