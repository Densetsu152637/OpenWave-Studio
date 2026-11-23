//! The peak pyramid: min and max over all channels per 64, 512 and 4096 frames, stored in
//! `cache/<hash>.owpk` (safe to delete; rebuilt from the audio file).
//!
//! # File layout (little-endian)
//!
//! | offset | content |
//! | --- | --- |
//! | 0 | magic `OWPK` |
//! | 4 | `u32` version, [`VERSION`] |
//! | 8 | `u32` sample rate |
//! | 12 | `u32` channels |
//! | 16 | `u64` frames of the audio file |
//! | 24 | `u32` level count, 3 |
//! | 28 | `u32` reserved, 0 |
//! | 32 | three `u32` block sizes, 64, 512, 4096 |
//! | 44 | per level in order: `ceil(frames / block)` entries of `f32` min, `f32` max |
//!
//! A file whose length does not match the header exactly is rejected, so a truncated or
//! hostile cache never yields a misread; callers rebuild it from the audio.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use crate::Pcm;

/// Format version of `.owpk` files.
pub const VERSION: u32 = 1;
/// Frames per entry of each level, finest first.
pub const BLOCKS: [usize; 3] = [64, 512, 4096];
/// Most bins one range query returns (matches `ow_ipc::peaks::MAX_BINS`).
pub const MAX_BINS: usize = 16_384;
const HEADER_BYTES: usize = 44;
/// Most frames a pyramid describes (keeps counts well inside `usize` on every target).
const MAX_FRAMES: u64 = 1 << 40;

/// The three levels of min/max pairs for one audio file.
#[derive(Debug, Clone, PartialEq)]
pub struct Pyramid {
    /// Sample rate of the audio.
    pub sample_rate: u32,
    /// Channels of the audio.
    pub channels: u32,
    /// Frames of the audio.
    pub frames: u64,
    /// Entries per level, finest first.
    pub levels: [Vec<(f32, f32)>; 3],
}

fn entries(frames: u64, block: usize) -> usize {
    frames.div_ceil(block as u64) as usize
}

fn fold(pairs: &[(f32, f32)]) -> (f32, f32) {
    pairs
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p.0), hi.max(p.1))
        })
}

impl Pyramid {
    fn from_level0(sample_rate: u32, channels: u32, frames: u64, level0: Vec<(f32, f32)>) -> Self {
        let coarser = |finer: &[(f32, f32)], ratio: usize| -> Vec<(f32, f32)> {
            finer.chunks(ratio).map(fold).collect()
        };
        let level1 = coarser(&level0, BLOCKS[1] / BLOCKS[0]);
        let level2 = coarser(&level1, BLOCKS[2] / BLOCKS[1]);
        Self {
            sample_rate,
            channels,
            frames,
            levels: [level0, level1, level2],
        }
    }

    /// The pyramid of in-memory audio.
    pub fn from_pcm(pcm: &Pcm) -> Self {
        let frames = pcm.frames();
        let level0 = (0..entries(frames as u64, BLOCKS[0]))
            .map(|i| {
                let range = i * BLOCKS[0]..((i + 1) * BLOCKS[0]).min(frames);
                pcm.channels
                    .iter()
                    .flat_map(|c| &c[range.clone()])
                    .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| {
                        (lo.min(*s), hi.max(*s))
                    })
            })
            .collect();
        Self::from_level0(
            pcm.sample_rate,
            pcm.channels.len() as u32,
            frames as u64,
            level0,
        )
    }

    /// The pyramid of a bundle WAV, read in one pass without holding the audio.
    pub fn from_wav(path: &Path) -> io::Result<Self> {
        let bad = io::Error::other;
        let mut reader = hound::WavReader::new(BufReader::new(File::open(path)?)).map_err(bad)?;
        let spec = reader.spec();
        let channels = usize::from(spec.channels).max(1);
        let frames = u64::from(reader.duration());
        let mut level0 = Vec::with_capacity(entries(frames, BLOCKS[0]));
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        let mut in_block = 0;
        let mut total = 0_u64;
        let push = |lo: &mut f32, hi: &mut f32, level0: &mut Vec<(f32, f32)>| {
            level0.push((*lo, *hi));
            *lo = f32::INFINITY;
            *hi = f32::NEG_INFINITY;
        };
        let mut sample_index = 0_usize;
        let mut each = |value: f32| {
            lo = lo.min(value);
            hi = hi.max(value);
            sample_index += 1;
            if sample_index == channels {
                sample_index = 0;
                in_block += 1;
                total += 1;
                if in_block == BLOCKS[0] {
                    in_block = 0;
                    push(&mut lo, &mut hi, &mut level0);
                }
            }
        };
        match (spec.sample_format, spec.bits_per_sample) {
            (hound::SampleFormat::Float, 32) => {
                for s in reader.samples::<f32>() {
                    each(s.map_err(bad)?);
                }
            }
            (hound::SampleFormat::Int, bits @ 1..=32) => {
                let scale = 1.0 / (2.0_f64.powi(i32::from(bits) - 1)) as f32;
                for s in reader.samples::<i32>() {
                    each(s.map_err(bad)? as f32 * scale);
                }
            }
            _ => return Err(io::Error::other("unsupported WAV format")),
        }
        if in_block > 0 {
            level0.push((lo, hi));
        }
        Ok(Self::from_level0(
            spec.sample_rate,
            channels as u32,
            total,
            level0,
        ))
    }

    /// Writes the `.owpk` file atomically (temp file, then rename).
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let mut bytes =
            Vec::with_capacity(HEADER_BYTES + 8 * self.levels.iter().map(Vec::len).sum::<usize>());
        bytes.extend_from_slice(b"OWPK");
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&self.sample_rate.to_le_bytes());
        bytes.extend_from_slice(&self.channels.to_le_bytes());
        bytes.extend_from_slice(&self.frames.to_le_bytes());
        bytes.extend_from_slice(&3_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        for block in BLOCKS {
            bytes.extend_from_slice(&(block as u32).to_le_bytes());
        }
        for level in &self.levels {
            for (lo, hi) in level {
                bytes.extend_from_slice(&lo.to_le_bytes());
                bytes.extend_from_slice(&hi.to_le_bytes());
            }
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("owpk.tmp");
        fs::write(&temp, bytes)?;
        fs::rename(&temp, path)
    }

    /// Reads and validates a `.owpk` file (`InvalidData` for anything not exactly well-formed).
    pub fn read(path: &Path) -> io::Result<Self> {
        let invalid = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
        let bytes = fs::read(path)?;
        let u32_at = |at: usize| {
            bytes
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        if bytes.len() < HEADER_BYTES || &bytes[..4] != b"OWPK" {
            return Err(invalid("not a peak file"));
        }
        if u32_at(4) != Some(VERSION) || u32_at(24) != Some(3) {
            return Err(invalid("unsupported peak file version"));
        }
        for (i, block) in BLOCKS.iter().enumerate() {
            if u32_at(32 + 4 * i) != Some(*block as u32) {
                return Err(invalid("unexpected block sizes"));
            }
        }
        let mut frames = [0_u8; 8];
        frames.copy_from_slice(&bytes[16..24]);
        let frames = u64::from_le_bytes(frames);
        if frames > MAX_FRAMES {
            return Err(invalid("implausible length"));
        }
        let counts = BLOCKS.map(|b| entries(frames, b));
        if bytes.len() != HEADER_BYTES + 8 * counts.iter().sum::<usize>() {
            return Err(invalid("length does not match header"));
        }
        let mut at = HEADER_BYTES;
        let levels = counts.map(|count| {
            (0..count)
                .map(|_| {
                    let f = |o: usize| {
                        f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]])
                    };
                    let pair = (f(at), f(at + 4));
                    at += 8;
                    pair
                })
                .collect::<Vec<_>>()
        });
        Ok(Self {
            sample_rate: u32_at(8).unwrap_or(0),
            channels: u32_at(12).unwrap_or(0),
            frames,
            levels,
        })
    }

    /// `bins` min/max pairs over frames `[start, end)` (clamped to the audio), using the
    /// coarsest level whose entries are no longer than one bin. Bins split the range evenly;
    /// where a bin's edges fall inside an entry the whole entry counts, so a bin never
    /// understates its peaks. Empty for an empty range or `bins == 0`.
    pub fn range(&self, start: u64, end: u64, bins: usize) -> Vec<(f32, f32)> {
        let end = end.min(self.frames);
        let bins = bins.min(MAX_BINS);
        if bins == 0 || start >= end {
            return Vec::new();
        }
        let len = end - start;
        let bins = bins.min(len as usize);
        let per_bin = len / bins as u64;
        let level = (0..3)
            .rev()
            .find(|l| BLOCKS[*l] as u64 <= per_bin)
            .unwrap_or(0);
        let block = BLOCKS[level] as u64;
        let data = &self.levels[level];
        (0..bins as u64)
            .map(|bin| {
                let from = start + bin * len / bins as u64;
                let to = start + (bin + 1) * len / bins as u64;
                let first = (from / block) as usize;
                let last = (to.div_ceil(block) as usize).min(data.len()).max(first + 1);
                fold(&data[first.min(data.len() - 1)..last])
            })
            .collect()
    }
}

/// Builds `<wav>`'s pyramid and writes it to `owpk` on a new thread (never the audio thread).
pub fn spawn_build(wav: PathBuf, owpk: PathBuf) -> JoinHandle<io::Result<()>> {
    thread::spawn(move || Pyramid::from_wav(&wav)?.write(&owpk))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Media id to loaded pyramid, for the `peaks_range` command. Files are read on first use and
/// kept; a media item without a (valid) peak file answers an empty range.
#[derive(Default)]
pub struct PeakCache {
    files: Mutex<HashMap<u64, PathBuf>>,
    loaded: Mutex<HashMap<u64, Arc<Pyramid>>>,
}

impl PeakCache {
    /// Points `media` at its `.owpk` file (dropping any pyramid loaded earlier for it).
    pub fn register(&self, media: u64, owpk: PathBuf) {
        lock(&self.files).insert(media, owpk);
        lock(&self.loaded).remove(&media);
    }

    /// Min/max pairs for the range (see [`Pyramid::range`]).
    pub fn range(&self, media: u64, start: u64, end: u64, bins: usize) -> Vec<(f32, f32)> {
        let cached = lock(&self.loaded).get(&media).cloned();
        let pyramid = cached.or_else(|| {
            let path = lock(&self.files).get(&media)?.clone();
            let pyramid = Arc::new(Pyramid::read(&path).ok()?);
            lock(&self.loaded).insert(media, Arc::clone(&pyramid));
            Some(pyramid)
        });
        pyramid.map_or_else(Vec::new, |p| p.range(start, end, bins))
    }
}
