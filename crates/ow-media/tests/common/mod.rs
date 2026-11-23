//! Helpers shared by the test files (each includes this with `mod common;`).
#![allow(dead_code)]

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

/// Deterministic test signal in `-1..1` for frame `i`, channel `c`: a slow sine plus
/// hash noise, so every sample differs and a misplaced block is detected.
pub fn signal(i: u64, c: usize) -> f32 {
    let hash = i.wrapping_mul(2_654_435_761).wrapping_add(c as u64 * 97);
    let noise = ((hash >> 7) & 0xFFFF) as f32 / 65_535.0 * 2.0 - 1.0;
    let tone = (i as f32 * 0.01 + c as f32).sin();
    0.5 * tone + 0.4 * noise
}

/// Writes `frames` frames of [`signal`] as a 32-bit float WAV.
pub fn write_signal(path: &Path, channels: u16, rate: u32, frames: u64) {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer =
        hound::WavWriter::new(BufWriter::new(File::create(path).unwrap()), spec).unwrap();
    for i in 0..frames {
        for c in 0..usize::from(channels) {
            writer.write_sample(signal(i, c)).unwrap();
        }
    }
    writer.finalize().unwrap();
}
