//! The peak pyramid and its `.owpk` file.

mod common;

use std::fs;

use common::{signal, write_signal};
use ow_media::Pcm;
use ow_media::peaks::{BLOCKS, PeakCache, Pyramid, spawn_build};

fn pcm(frames: usize) -> Pcm {
    Pcm {
        sample_rate: 48_000,
        channels: (0..2)
            .map(|c| (0..frames as u64).map(|i| signal(i, c)).collect())
            .collect(),
    }
}

/// Reference: min/max of both channels over `[from, to)`.
fn naive(pcm: &Pcm, from: usize, to: usize) -> (f32, f32) {
    pcm.channels
        .iter()
        .flat_map(|c| &c[from..to])
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| {
            (lo.min(*s), hi.max(*s))
        })
}

#[test]
fn levels_match_a_direct_scan_and_round_trip_through_the_file() {
    let pcm = pcm(50_000);
    let pyramid = Pyramid::from_pcm(&pcm);
    for (level, block) in BLOCKS.iter().enumerate() {
        assert_eq!(pyramid.levels[level].len(), 50_000_usize.div_ceil(*block));
        for (i, entry) in pyramid.levels[level].iter().enumerate() {
            let to = ((i + 1) * block).min(50_000);
            assert_eq!(
                *entry,
                naive(&pcm, i * block, to),
                "level {level} entry {i}"
            );
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache/x.owpk");
    pyramid.write(&path).unwrap();
    assert_eq!(Pyramid::read(&path).unwrap(), pyramid);
}

#[test]
fn range_uses_the_coarsest_fitting_level_and_never_understates() {
    let pcm = pcm(100_000);
    let pyramid = Pyramid::from_pcm(&pcm);
    // Aligned bins are exact at every level.
    for (bin_frames, bins) in [(64_usize, 100_usize), (512, 100), (4096, 20)] {
        let got = pyramid.range(0, (bin_frames * bins) as u64, bins);
        assert_eq!(got.len(), bins);
        for (b, pair) in got.iter().enumerate() {
            assert_eq!(*pair, naive(&pcm, b * bin_frames, (b + 1) * bin_frames));
        }
    }
    // Unaligned bins cover at least the true range.
    let got = pyramid.range(1000, 90_001, 37);
    let len = 89_001;
    for (b, pair) in got.iter().enumerate() {
        let from = 1000 + b * len / 37;
        let to = 1000 + (b + 1) * len / 37;
        let exact = naive(&pcm, from, to);
        assert!(pair.0 <= exact.0 && pair.1 >= exact.1, "bin {b}");
    }
    // Degenerate requests.
    assert!(pyramid.range(5, 5, 10).is_empty());
    assert!(pyramid.range(200_000, 300_000, 10).is_empty());
    assert!(pyramid.range(0, 100, 0).is_empty());
    assert_eq!(pyramid.range(0, 1_000_000, 100_000).len(), 16_384);
}

#[test]
fn building_from_the_wav_matches_the_in_memory_pyramid_off_thread() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("a.wav");
    write_signal(&wav, 2, 48_000, 30_001);
    let owpk = dir.path().join("cache").join("a.owpk");
    spawn_build(wav, owpk.clone()).join().unwrap().unwrap();
    let from_file = Pyramid::read(&owpk).unwrap();
    assert_eq!(from_file.frames, 30_001);
    assert_eq!(from_file.channels, 2);
    assert_eq!(from_file.levels, Pyramid::from_pcm(&pcm(30_001)).levels);
}

#[test]
fn damaged_files_are_rejected_not_misread() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.owpk");
    Pyramid::from_pcm(&pcm(10_000)).write(&path).unwrap();
    let good = fs::read(&path).unwrap();
    let mut cases: Vec<Vec<u8>> =
        vec![vec![], good[..20].to_vec(), good[..good.len() - 1].to_vec()];
    let mut extra = good.clone();
    extra.push(0);
    cases.push(extra);
    for (offset, value) in [(0_usize, 0x58_u8), (4, 9), (24, 4), (32, 65), (23, 0xFF)] {
        let mut bad = good.clone();
        bad[offset] = value;
        cases.push(bad);
    }
    for (i, bytes) in cases.iter().enumerate() {
        fs::write(&path, bytes).unwrap();
        assert!(Pyramid::read(&path).is_err(), "case {i}");
    }
    // The cache treats an unreadable file as no peaks.
    let cache = PeakCache::default();
    cache.register(1, path);
    assert!(cache.range(1, 0, 100, 10).is_empty());
    assert!(cache.range(2, 0, 100, 10).is_empty());
}

#[test]
fn registering_again_replaces_a_loaded_pyramid() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.owpk"), dir.path().join("b.owpk"));
    let quiet = Pcm {
        sample_rate: 48_000,
        channels: vec![vec![0.1; 640]],
    };
    let loud = Pcm {
        sample_rate: 48_000,
        channels: vec![vec![0.9; 640]],
    };
    Pyramid::from_pcm(&quiet).write(&a).unwrap();
    Pyramid::from_pcm(&loud).write(&b).unwrap();
    let cache = PeakCache::default();
    cache.register(1, a);
    assert_eq!(cache.range(1, 0, 640, 1), [(0.1, 0.1)]);
    cache.register(1, b);
    assert_eq!(cache.range(1, 0, 640, 1), [(0.9, 0.9)]);
}
