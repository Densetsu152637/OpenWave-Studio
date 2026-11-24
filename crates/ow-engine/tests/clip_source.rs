//! The clip source node: placement, gain, loops, streamed and in-memory sources, and the
//! engine cycle's real-time guard.
//!
//! `AllocDisabler` is this binary's global allocator and `AudioCore::process` runs every cycle
//! under `assert_no_alloc`, so each render below also proves the node and the sources it reads
//! allocate nothing on the audio thread (debug builds only: the guard is compiled out of
//! release builds).

use std::sync::Arc;
use std::time::{Duration, Instant};

use assert_no_alloc::AllocDisabler;
use ow_engine::graph::nodes::MasterNode;
use ow_engine::{
    ClipPlacement, ClipSourceNode, Graph, MAX_BLOCK, Message, OfflineRenderer, TransportCommand,
};
use ow_media::{AudioSource, MemorySource, Pcm, StreamOptions, StreamSource};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const RATE: u32 = 48_000;
const FRAMES: usize = 20_000;

fn sample(i: usize, channel: usize) -> f32 {
    ((i * 31 + channel * 7) % 1999) as f32 / 1000.0 - 1.0
}

fn pcm() -> Pcm {
    Pcm {
        sample_rate: RATE,
        channels: (0..2)
            .map(|c| (0..FRAMES).map(|i| sample(i, c)).collect())
            .collect(),
    }
}

fn renderer(source: Box<dyn AudioSource>, placements: Vec<ClipPlacement>) -> OfflineRenderer {
    let mut graph = Graph::new();
    let clip = graph.add_node(Box::new(ClipSourceNode::new(source, placements)));
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(clip, 0, master, 0).unwrap();
    graph.connect(clip, 1, master, 1).unwrap();
    graph.set_master(master).unwrap();
    OfflineRenderer::new(graph.compile(RATE, MAX_BLOCK).unwrap(), 2)
}

fn play(renderer: &mut OfflineRenderer) {
    renderer.send(Message::Transport(TransportCommand::Play));
}

const PLACEMENT: ClipPlacement = ClipPlacement {
    start: 1000,
    length: 3000,
    source_offset: 500,
    gain: 0.5,
};

/// What the placement alone produces at timeline sample `t`.
fn expected(t: usize, channel: usize) -> f32 {
    let start = PLACEMENT.start as usize;
    if t < start || t >= start + PLACEMENT.length as usize {
        return 0.0;
    }
    sample(PLACEMENT.source_offset as usize + t - start, channel) * PLACEMENT.gain
}

#[test]
fn a_clip_plays_at_its_position_in_every_block_size() {
    for block in [1, 7, 64, 480, 1000, 4096] {
        let mut r = renderer(
            Box::new(MemorySource::new(Arc::new(pcm()))),
            vec![PLACEMENT],
        );
        // Stopped: silence.
        assert!(r.render(256, block).iter().all(|s| *s == 0.0));
        r.send(Message::Transport(TransportCommand::Locate(0)));
        play(&mut r);
        let out = r.render(6000, block);
        for t in 0..6000 {
            for c in 0..2 {
                assert_eq!(out[t * 2 + c], expected(t, c), "block {block} t {t} c {c}");
            }
        }
    }
}

#[test]
fn overlapping_placements_sum_and_a_loop_replays_the_clip() {
    let second = ClipPlacement {
        start: 2000,
        length: 500,
        source_offset: 0,
        gain: 1.0,
    };
    let mut r = renderer(
        Box::new(MemorySource::new(Arc::new(pcm()))),
        vec![PLACEMENT, second],
    );
    let beats = |samples: u64| samples as f64 / 24_000.0;
    r.send(Message::Transport(TransportCommand::SetLoop {
        enabled: true,
        start_beats: beats(500),
        end_beats: beats(3000),
    }));
    r.send(Message::Transport(TransportCommand::Locate(500)));
    play(&mut r);
    let out = r.render(7500, 480);
    // The playhead walks 500..3000 and wraps back to 500.
    for (frame, t) in (0..7500).map(|f| (f, 500 + f % 2500)) {
        for c in 0..2 {
            let mut want = expected(t, c);
            if (2000..2500).contains(&t) {
                want += sample(t - 2000, c);
            }
            assert_eq!(out[frame * 2 + c], want, "frame {frame} (sample {t})");
        }
    }
}

#[test]
fn a_streamed_source_renders_the_same_as_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.wav");
    ow_media::import::write_wav(&path, &pcm()).unwrap();
    let stream = StreamSource::open(&path, StreamOptions::default()).unwrap();
    let handle = stream.handle();
    // The clip starts at source frame 500, not 0: tell the disk thread before playing, as the
    // control thread will before it starts the transport.
    handle.seek_hint(PLACEMENT.source_offset);
    let deadline = Instant::now() + Duration::from_secs(30);
    while handle.prefetched_to() < FRAMES as u64 {
        assert!(Instant::now() < deadline, "disk thread stalled");
        std::thread::yield_now();
    }
    let mut streamed = renderer(Box::new(stream), vec![PLACEMENT]);
    let mut memory = renderer(
        Box::new(MemorySource::new(Arc::new(pcm()))),
        vec![PLACEMENT],
    );
    play(&mut streamed);
    play(&mut memory);
    let want = memory.render(5000, 128);
    assert_eq!(streamed.render(5000, 128), want);
    assert!(want.iter().any(|s| *s != 0.0));
    assert_eq!(handle.underruns(), 0);
}
