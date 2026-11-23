//! Streaming from disk renders exactly what in-memory playback renders.
//!
//! The 30-minute file is generated procedurally into a temp directory (mono, 8 kHz: 14.4 M
//! frames, 58 MB) and compared block by block against a [`MemorySource`] over the same file.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{signal, write_signal};
use ow_media::source::load_wav;
use ow_media::{
    AudioSource, MemorySource, SourceConfig, StreamHandle, StreamOptions, StreamSource, open_source,
};

const BLOCK: usize = 512;

/// Waits until the disk thread has read up to `frame` (or the end of the file).
fn wait_prefetched(handle: &StreamHandle, frame: u64, total: u64) {
    let target = frame.min(total);
    let deadline = Instant::now() + Duration::from_secs(30);
    while handle.prefetched_to() < target {
        assert!(Instant::now() < deadline, "disk thread stalled");
        std::thread::yield_now();
    }
}

fn blocks(channels: usize) -> Vec<Vec<f32>> {
    vec![vec![0.0; BLOCK]; channels]
}

#[test]
fn thirty_minute_file_streams_identically_to_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long.wav");
    let frames: u64 = 30 * 60 * 8000;
    write_signal(&path, 1, 8000, frames);

    let mut memory = MemorySource::new(Arc::new(load_wav(&path).unwrap()));
    let mut stream = StreamSource::open(&path, StreamOptions::default()).unwrap();
    let handle = stream.handle();
    assert_eq!(stream.frames(), frames);

    let (mut want, mut got) = (blocks(1), blocks(1));
    let mut position = 0;
    while position < frames + BLOCK as u64 {
        wait_prefetched(&handle, position + BLOCK as u64, frames);
        memory.read(position, &mut want, BLOCK);
        stream.read(position, &mut got, BLOCK);
        assert!(want == got, "blocks differ at frame {position}");
        position += BLOCK as u64;
    }
    // Spot-check the generator too, so "identical" is not "identically wrong".
    memory.read(123_456, &mut want, BLOCK);
    assert_eq!(want[0][7], signal(123_463, 0));
    assert_eq!(handle.underruns(), 0);

    // Seeks with a hint find their data; a seek back to the start too.
    for target in [10_000_000_u64, 777, 14_399_000, 0] {
        handle.seek_hint(target);
        wait_prefetched(&handle, target + BLOCK as u64, frames);
        memory.read(target, &mut want, BLOCK);
        stream.read(target, &mut got, BLOCK);
        assert!(want == got, "seek to {target} differs");
    }
    assert_eq!(handle.underruns(), 0);
}

#[test]
fn an_underrun_is_silence_and_counted_then_playback_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("short.wav");
    write_signal(&path, 2, 8000, 200_000);
    let mut memory = MemorySource::new(Arc::new(load_wav(&path).unwrap()));
    let mut stream = StreamSource::open(&path, StreamOptions::default()).unwrap();
    let handle = stream.handle();
    let (mut want, mut got) = (blocks(2), blocks(2));

    wait_prefetched(&handle, BLOCK as u64, 200_000);
    stream.read(0, &mut got, BLOCK);
    assert_eq!(handle.underruns(), 0);

    // Seek without a hint while the disk thread is stalled: silence, one underrun.
    handle.pause_prefetch(true);
    for channel in &mut got {
        channel.fill(1.0);
    }
    stream.read(150_000, &mut got, BLOCK);
    assert!(got.iter().all(|c| c.iter().all(|s| *s == 0.0)));
    assert_eq!(handle.underruns(), 1);

    // Once the disk thread resumes, the next block continues from the data.
    handle.pause_prefetch(false);
    wait_prefetched(&handle, 150_000 + 2 * BLOCK as u64, 200_000);
    memory.read(150_000 + BLOCK as u64, &mut want, BLOCK);
    stream.read(150_000 + BLOCK as u64, &mut got, BLOCK);
    assert!(want == got);
    assert_eq!(handle.underruns(), 1);

    // Reading past the end is silence and not an underrun.
    handle.seek_hint(200_000 - 10);
    wait_prefetched(&handle, 200_000, 200_000);
    stream.read(200_000 - 10, &mut got, BLOCK);
    assert!(got[0][10..].iter().all(|s| *s == 0.0));
    stream.read(1_000_000, &mut got, BLOCK);
    assert!(got[0].iter().all(|s| *s == 0.0));
    assert_eq!(handle.underruns(), 1);
}

#[test]
fn short_files_play_from_memory_and_long_ones_stream() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.wav");
    write_signal(&path, 1, 1000, 10_000); // 10 s
    let (_, stream) = open_source(&path, &SourceConfig::default()).unwrap();
    assert!(stream.is_none(), "10 s is under the default 30 s threshold");
    let config = SourceConfig {
        stream_threshold_secs: 5.0,
        ..SourceConfig::default()
    };
    let (source, stream) = open_source(&path, &config).unwrap();
    assert!(stream.is_some(), "10 s is over a 5 s threshold");
    assert_eq!(source.frames(), 10_000);
}
