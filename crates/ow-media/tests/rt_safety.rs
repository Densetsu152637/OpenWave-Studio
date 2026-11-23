//! The audio-thread paths allocate nothing: `AllocDisabler` is the global allocator and every
//! call below runs under `assert_no_alloc`, which aborts on any allocation or free.

#[cfg(not(debug_assertions))]
compile_error!(
    "rt_safety needs debug assertions: assert_no_alloc is compiled out of release builds, so \
     these tests would pass without checking anything. Run them in the dev or test profile."
);

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use common::write_signal;
use ow_media::source::load_wav;
use ow_media::{AudioSource, MemorySource, RecordWriter, StreamOptions, StreamSource};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn stream_and_memory_reads_do_not_allocate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.wav");
    write_signal(&path, 2, 8000, 400_000);
    let mut memory = MemorySource::new(Arc::new(load_wav(&path).unwrap()));
    let options = StreamOptions {
        block_frames: 1024,
        blocks: 16,
    };
    let mut stream = StreamSource::open(&path, options).unwrap();
    let handle = stream.handle();
    let mut out = vec![vec![0.0_f32; 256]; 2];

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut position = 0;
    // Contiguous playback, with waits outside the guarded call so the data is there.
    while position < 300_000 {
        while handle.prefetched_to() < position + 256 {
            assert!(Instant::now() < deadline, "disk thread stalled");
            std::thread::yield_now();
        }
        assert_no_alloc(|| {
            stream.read(position, &mut out, 256);
            memory.read(position, &mut out, 256);
        });
        position += 256;
    }
    // A seek without data (the underrun path), a hinted seek and the end of the file.
    assert_no_alloc(|| stream.read(5, &mut out, 256));
    handle.seek_hint(399_900);
    std::thread::sleep(Duration::from_millis(50));
    assert_no_alloc(|| {
        stream.read(399_900, &mut out, 256);
        stream.read(500_000, &mut out, 256);
        memory.read(399_900, &mut out, 256);
    });
}

#[test]
fn recording_pushes_do_not_allocate() {
    let dir = tempfile::tempdir().unwrap();
    let (writer, mut producer) =
        RecordWriter::start(&dir.path().join("r.wav"), 2, 48_000, 4096).unwrap();
    let block = vec![0.25_f32; 2 * 128];
    for _ in 0..100 {
        assert_no_alloc(|| {
            producer.push(&block);
        });
        std::thread::sleep(Duration::from_millis(1));
    }
    let stats = writer.finish().unwrap();
    assert!(stats.frames > 0);
}
