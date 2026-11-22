//! High-rate telemetry: the [`TelemetryBank`] the audio thread writes and the packer the UI
//! bridge thread samples.
//!
//! The bank is a block of preallocated atomics. The audio thread (and its DSP workers) only
//! ever store to it with relaxed atomic operations: no allocation, no lock, no syscall, so
//! writing meters from inside a node is as real-time safe as the node itself. The UI bridge
//! thread reads it at up to 60 Hz ([`FramePacker::pack`]) and sends the bytes to the webview
//! through a Tauri `ipc::Channel` as a raw binary message, never JSON.
//!
//! # Frame layout (version 1)
//!
//! A frame is a sequence of little-endian `f32` words (4 bytes each), `9 + 5 * n` words long,
//! where `n` is the number of subscribed strips. Counters that do not fit an `f32` exactly are
//! carried as integer-valued words below 2^24 so the UI decodes them with plain `getFloat32`
//! (`src/lib/ipc/telemetry.ts` mirrors this table).
//!
//! | word | content |
//! | --- | --- |
//! | 0 | frame format version, `1.0` |
//! | 1 | frame counter, wraps at 2^24 (the UI detects dropped frames by gaps) |
//! | 2 | transport position in sample frames, high part: `position >> 24` |
//! | 3 | transport position, low part: `position & 0xFF_FFFF` |
//! | 4 | callback CPU load: 0.0 idle, 1.0 the whole buffer period |
//! | 5 | control messages dropped because a producer ring was full (saturates at 2^24 - 1) |
//! | 6 | control messages addressed to a node not in the live plan (saturates likewise) |
//! | 7 | MIDI events dropped at the input rings (saturates likewise) |
//! | 8 | `n`, the number of strip records that follow |
//! | `9 + 5 i` | strip record `i`: meter slot index |
//! | `10 + 5 i` | peak left (linear, the maximum since the previous frame) |
//! | `11 + 5 i` | peak right |
//! | `12 + 5 i` | RMS left of the latest block |
//! | `13 + 5 i` | RMS right |
//!
//! Position in frames is `hi * 2^24 + lo`, exact up to 2^48 frames. Strip records appear in
//! ascending slot order and only for strips the UI subscribed to ([`Subscriptions`]); a slot
//! outside the bank is never sent. Meter slots are assigned by [`meter_slot`]: the master is
//! slot 0, then tracks in display order, then buses.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use ow_model::{MixerRef, Project};

/// Meter slots in the bank (strips beyond that are not metered).
pub const METER_SLOTS: usize = 256;
/// Slot of the master strip.
pub const MASTER_SLOT: usize = 0;
/// Frame format version carried in word 0.
pub const FRAME_VERSION: f32 = 1.0;
/// Words before the strip records.
pub const HEADER_WORDS: usize = 9;
/// Words per strip record.
pub const STRIP_WORDS: usize = 5;
/// Largest integer-valued counter a frame carries exactly.
pub const COUNTER_MAX: u32 = (1 << 24) - 1;
/// Largest possible frame in bytes.
pub const MAX_FRAME_BYTES: usize = (HEADER_WORDS + STRIP_WORDS * METER_SLOTS) * 4;

/// Which overflow counter to set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    /// A producer ring was full and a control message was dropped.
    Ring = 0,
    /// A message addressed a node that is not in the live plan.
    Unrouted = 1,
    /// A MIDI event was dropped at an input ring.
    Midi = 2,
}

#[derive(Debug, Default)]
struct MeterCell {
    peak_left: AtomicU32,
    peak_right: AtomicU32,
    rms_left: AtomicU32,
    rms_right: AtomicU32,
}

/// Preallocated telemetry atomics: written by the audio thread, sampled by the UI bridge thread.
///
/// Peaks accumulate (maximum since the last read) so a short peak between two 60 Hz samples is
/// not lost; reading resets them.
#[derive(Debug)]
pub struct TelemetryBank {
    meters: Box<[MeterCell]>,
    position: AtomicU64,
    cpu: AtomicU32,
    overflow: [AtomicU64; 3],
}

impl Default for TelemetryBank {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryBank {
    /// A bank with [`METER_SLOTS`] meters, all zero.
    pub fn new() -> Self {
        Self {
            meters: (0..METER_SLOTS).map(|_| MeterCell::default()).collect(),
            position: AtomicU64::new(0),
            cpu: AtomicU32::new(0),
            overflow: [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)],
        }
    }

    /// Publishes one block's meter values for `slot`: the peaks fold into the running maximum, the
    /// RMS values replace the previous block's. Ignores a slot outside the bank. Audio thread.
    pub fn write_meter(&self, slot: usize, peak: [f32; 2], rms: [f32; 2]) {
        let Some(cell) = self.meters.get(slot) else {
            return;
        };
        // Non-negative floats order like their bit patterns, so a max over the bits is a max over
        // the values. NaN and negative inputs are dropped.
        for (atomic, value) in [(&cell.peak_left, peak[0]), (&cell.peak_right, peak[1])] {
            if value >= 0.0 {
                atomic.fetch_max(value.to_bits(), Ordering::Relaxed);
            }
        }
        cell.rms_left.store(rms[0].to_bits(), Ordering::Relaxed);
        cell.rms_right.store(rms[1].to_bits(), Ordering::Relaxed);
    }

    /// Publishes the transport position in sample frames. Audio thread.
    pub fn set_position(&self, frames: u64) {
        self.position.store(frames, Ordering::Relaxed);
    }

    /// Publishes the smoothed callback load. Audio thread.
    pub fn set_cpu_load(&self, load: f32) {
        self.cpu.store(load.to_bits(), Ordering::Relaxed);
    }

    /// Publishes an overflow counter's running total. Any thread.
    pub fn set_overflow(&self, kind: Overflow, total: u64) {
        self.overflow[kind as usize].store(total, Ordering::Relaxed);
    }

    /// The published transport position in frames.
    pub fn position(&self) -> u64 {
        self.position.load(Ordering::Relaxed)
    }

    /// The published callback load.
    pub fn cpu_load(&self) -> f32 {
        f32::from_bits(self.cpu.load(Ordering::Relaxed))
    }

    /// The published total of an overflow counter.
    pub fn overflow(&self, kind: Overflow) -> u64 {
        self.overflow[kind as usize].load(Ordering::Relaxed)
    }

    /// Reads one meter and resets its peaks: `(peak, rms)` per channel.
    pub fn take_meter(&self, slot: usize) -> Option<([f32; 2], [f32; 2])> {
        let cell = self.meters.get(slot)?;
        Some((
            [
                f32::from_bits(cell.peak_left.swap(0, Ordering::Relaxed)),
                f32::from_bits(cell.peak_right.swap(0, Ordering::Relaxed)),
            ],
            [
                f32::from_bits(cell.rms_left.load(Ordering::Relaxed)),
                f32::from_bits(cell.rms_right.load(Ordering::Relaxed)),
            ],
        ))
    }
}

/// The meter slot of a strip: the master is slot 0, tracks follow in display order, then buses.
/// `None` when the strip is not in the project or would fall beyond [`METER_SLOTS`].
///
/// Reordering tracks renumbers slots; the UI derives them from its mirror with the same rule
/// (`meterSlot` in `src/lib/ipc/telemetry.ts`).
pub fn meter_slot(project: &Project, strip: MixerRef) -> Option<usize> {
    let slot = match strip {
        MixerRef::Master => MASTER_SLOT,
        MixerRef::Track(id) => 1 + project.tracks.iter().position(|t| t.id == id)?,
        MixerRef::Bus(id) => {
            1 + project.tracks.len() + project.buses.iter().position(|b| b.id == id)?
        }
    };
    (slot < METER_SLOTS).then_some(slot)
}

/// The set of meter slots the UI wants (the visible strips), shared between the command thread
/// that changes it and the bridge thread that reads it, without a lock.
#[derive(Debug, Default)]
pub struct Subscriptions {
    bits: [AtomicU64; METER_SLOTS / 64],
}

impl Subscriptions {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the set with `slots` (slots beyond the bank are ignored).
    pub fn set(&self, slots: &[u16]) {
        let mut words = [0_u64; METER_SLOTS / 64];
        for slot in slots {
            let slot = usize::from(*slot);
            if slot < METER_SLOTS {
                words[slot / 64] |= 1 << (slot % 64);
            }
        }
        for (atomic, word) in self.bits.iter().zip(words) {
            atomic.store(word, Ordering::Relaxed);
        }
    }

    /// True when `slot` is subscribed.
    pub fn contains(&self, slot: usize) -> bool {
        slot < METER_SLOTS && self.bits[slot / 64].load(Ordering::Relaxed) & (1 << (slot % 64)) != 0
    }

    /// Number of subscribed slots.
    pub fn len(&self) -> usize {
        self.bits
            .iter()
            .map(|word| word.load(Ordering::Relaxed).count_ones() as usize)
            .sum()
    }

    /// True when nothing is subscribed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Builds frames into a reusable buffer, so sampling allocates nothing after construction.
#[derive(Debug)]
pub struct FramePacker {
    counter: u32,
    bytes: Vec<u8>,
}

impl Default for FramePacker {
    fn default() -> Self {
        Self::new()
    }
}

impl FramePacker {
    /// A packer with room for the largest possible frame.
    pub fn new() -> Self {
        Self {
            counter: 0,
            bytes: Vec::with_capacity(MAX_FRAME_BYTES),
        }
    }

    /// Samples the bank and packs one frame (resetting the subscribed peaks). The returned bytes
    /// are valid until the next call.
    pub fn pack(&mut self, bank: &TelemetryBank, subscribed: &Subscriptions) -> &[u8] {
        self.bytes.clear();
        self.counter = (self.counter + 1) & COUNTER_MAX;
        let position = bank.position();
        let saturate = |total: u64| total.min(u64::from(COUNTER_MAX)) as f32;
        self.word(FRAME_VERSION);
        self.word(self.counter as f32);
        self.word((position >> 24) as f32);
        self.word((position & u64::from(COUNTER_MAX)) as f32);
        self.word(bank.cpu_load());
        self.word(saturate(bank.overflow(Overflow::Ring)));
        self.word(saturate(bank.overflow(Overflow::Unrouted)));
        self.word(saturate(bank.overflow(Overflow::Midi)));
        self.word(subscribed.len() as f32);
        for slot in 0..METER_SLOTS {
            if !subscribed.contains(slot) {
                continue;
            }
            let (peak, rms) = bank.take_meter(slot).unwrap_or_default();
            self.word(slot as f32);
            self.word(peak[0]);
            self.word(peak[1]);
            self.word(rms[0]);
            self.word(rms[1]);
        }
        &self.bytes
    }

    fn word(&mut self, value: f32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
}

/// One strip's record in a decoded frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StripMeter {
    /// Meter slot.
    pub slot: u32,
    /// Peak per channel since the previous frame.
    pub peak: [f32; 2],
    /// RMS per channel of the latest block.
    pub rms: [f32; 2],
}

/// A decoded telemetry frame (the Rust twin of `decodeFrame` in `src/lib/ipc/telemetry.ts`).
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Frame counter (wraps at 2^24).
    pub counter: u32,
    /// Transport position in sample frames.
    pub position: u64,
    /// Callback load.
    pub cpu_load: f32,
    /// Dropped control messages.
    pub ring_overflow: u32,
    /// Unrouted control messages.
    pub unrouted: u32,
    /// Dropped MIDI events.
    pub midi_dropped: u32,
    /// Strip records in ascending slot order.
    pub strips: Vec<StripMeter>,
}

impl Frame {
    /// Decodes a frame; `None` when the bytes are not a well-formed version-1 frame.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if !bytes.len().is_multiple_of(4) || bytes.len() < HEADER_WORDS * 4 {
            return None;
        }
        let word = |i: usize| -> f32 {
            f32::from_le_bytes([
                bytes[4 * i],
                bytes[4 * i + 1],
                bytes[4 * i + 2],
                bytes[4 * i + 3],
            ])
        };
        if word(0) != FRAME_VERSION {
            return None;
        }
        let count = word(8) as usize;
        if bytes.len() != (HEADER_WORDS + STRIP_WORDS * count) * 4 {
            return None;
        }
        let strips = (0..count)
            .map(|i| {
                let at = HEADER_WORDS + STRIP_WORDS * i;
                StripMeter {
                    slot: word(at) as u32,
                    peak: [word(at + 1), word(at + 2)],
                    rms: [word(at + 3), word(at + 4)],
                }
            })
            .collect();
        Some(Self {
            counter: word(1) as u32,
            position: ((word(2) as u64) << 24) | word(3) as u64,
            cpu_load: word(4),
            ring_overflow: word(5) as u32,
            unrouted: word(6) as u32,
            midi_dropped: word(7) as u32,
            strips,
        })
    }
}
