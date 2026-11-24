//! Bundle WAV headers are not trusted: forged channel counts and lengths are refused or
//! clamped before anything is allocated.

mod common;

use std::fs;
use std::path::Path;

use common::write_signal;
use ow_media::peaks::Pyramid;
use ow_media::source::load_wav;
use ow_media::{StreamOptions, StreamSource};

/// A small valid stereo file with its header rewritten by `patch`.
fn forged(dir: &Path, name: &str, patch: impl FnOnce(&mut Vec<u8>)) -> std::path::PathBuf {
    let path = dir.join(name);
    write_signal(&path, 2, 8000, 1000);
    let mut bytes = fs::read(&path).unwrap();
    patch(&mut bytes);
    fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn a_header_claiming_too_many_channels_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    // 64 channels, with a consistent block align and byte rate so the header itself parses.
    let path = forged(dir.path(), "wide.wav", |b| {
        b[22..24].copy_from_slice(&64_u16.to_le_bytes());
        b[28..32].copy_from_slice(&(8000_u32 * 256).to_le_bytes());
        b[32..34].copy_from_slice(&256_u16.to_le_bytes());
    });
    assert!(load_wav(&path).is_err());
    assert!(StreamSource::open(&path, StreamOptions::default()).is_err());
    assert!(Pyramid::from_wav(&path).is_err());
}

#[test]
fn a_header_claiming_more_frames_than_the_file_holds_is_clamped() {
    let dir = tempfile::tempdir().unwrap();
    let path = forged(dir.path(), "long.wav", |b| {
        b[40..44].copy_from_slice(&0xFFFF_FFF0_u32.to_le_bytes());
    });
    let pcm = load_wav(&path).unwrap();
    assert_eq!(pcm.frames(), 1000);
    let stream = StreamSource::open(&path, StreamOptions::default()).unwrap();
    assert_eq!(ow_media::AudioSource::frames(&stream), 1000);
    assert_eq!(Pyramid::from_wav(&path).unwrap().frames, 1000);
}

#[test]
fn media_that_is_not_32_bit_float_is_refused_for_playback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("int.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 8000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for i in 0..100 {
        w.write_sample(i as i16).unwrap();
    }
    w.finalize().unwrap();
    assert!(load_wav(&path).is_err());
    assert!(StreamSource::open(&path, StreamOptions::default()).is_err());
}
