//! Transport, tempo map and block timing, end to end through the engine cycle.
//!
//! Probe nodes write what they see in `ProcessContext::timing` into their output, and the tests
//! compare that against references computed independently (a per-sample model of loop playback,
//! the tempo map's own conversions), across block sizes (including a loop shorter than a block
//! and a loop boundary in the middle of a block) and with 0 and 8 DSP workers.
//!
//! `AllocDisabler` is this binary's global allocator, so `rt_safe_transport_traffic` proves the
//! transport additions allocate nothing on the audio thread (like `rt_safety`, it needs debug
//! assertions: `assert_no_alloc` is compiled out of release builds).

use std::f64::consts::TAU;

use assert_no_alloc::AllocDisabler;
use ow_engine::graph::nodes::MasterNode;
use ow_engine::{
    CoreBuilder, Graph, MAX_BLOCK, Message, MetronomeNode, Node, NodeId, OfflineRenderer,
    ProcessContext, RenderPlan, Timeline, TransportCommand,
};
use ow_model::time::{TempoMap, TempoPoint, TimeSignature, TimeSignatureMap};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

/// An optional loop `[start, end)` in samples.
type LoopRange = Option<(u64, u64)>;

const RATE: u32 = 48_000;
/// Samples per beat at the default 120 bpm.
const SPB: f64 = 24_000.0;
const WORKERS: [usize; 2] = [0, 8];
const BLOCKS: [usize; 9] = [1, 7, 64, 100, 128, 480, 1000, 4096, 5000];

fn beats(samples: u64) -> f64 {
    samples as f64 / SPB
}

fn transport(command: TransportCommand) -> Message {
    Message::Transport(command)
}

fn set_loop(start: u64, end: u64) -> Message {
    transport(TransportCommand::SetLoop {
        enabled: true,
        start_beats: beats(start),
        end_beats: beats(end),
    })
}

/// Writes the timeline sample of every frame the transport plays, and -1 elsewhere.
struct SampleProbe;

impl Node for SampleProbe {
    fn name(&self) -> &'static str {
        "sample-probe"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let timing = ctx.timing;
        let out = ctx.outputs.channel_mut(0);
        out.fill(-1.0);
        let mut next_frame = timing.count_in_frames();
        for span in timing.spans() {
            // Spans tile the playing frames: no gap, no overlap.
            assert_eq!(span.frame(), next_frame);
            next_frame = span.frame() + span.frames();
            for frame in span.frame_range() {
                out[frame] = span.sample_at(frame) as f32;
            }
        }
        assert!(next_frame == timing.frames() || timing.spans().next().is_none());
    }
}

/// Writes 1.0 on the frames the transport reports as recording, 0.0 elsewhere.
struct RecordProbe;

impl Node for RecordProbe {
    fn name(&self) -> &'static str {
        "record-probe"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let timing = ctx.timing;
        let out = ctx.outputs.channel_mut(0);
        out.fill(0.0);
        for span in timing.spans() {
            if let Some(range) = span.record_frames() {
                out[range].fill(1.0);
            }
        }
    }
}

/// Writes 1.0 on the frame each whole beat is placed on by `Span::frame_of_beat`.
struct BeatProbe;

impl Node for BeatProbe {
    fn name(&self) -> &'static str {
        "beat-probe"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let timing = ctx.timing;
        let out = ctx.outputs.channel_mut(0);
        out.fill(0.0);
        for span in timing.spans() {
            let first = span.start_beat().floor() as i64 - 1;
            let last = span.end_beat().ceil() as i64 + 1;
            for beat in first.max(0)..=last {
                if let Some(frame) = span.frame_of_beat(beat as f64) {
                    out[frame] += 1.0;
                }
            }
        }
    }
}

/// A one-node plan: `node` into both master inputs.
fn plan_of(node: Box<dyn Node>) -> (RenderPlan, NodeId) {
    let mut graph = Graph::new();
    let id = graph.add_node(node);
    let master = graph.add_node(Box::new(MasterNode::new()));
    graph.connect(id, 0, master, 0).unwrap();
    graph.connect(id, 0, master, 1).unwrap();
    graph.set_master(master).unwrap();
    (graph.compile(RATE, MAX_BLOCK).unwrap(), id)
}

fn renderer(node: Box<dyn Node>, workers: usize) -> OfflineRenderer {
    OfflineRenderer::with_workers(plan_of(node).0, 2, workers)
}

/// Left channel of an interleaved stereo render.
fn left(out: &[f32]) -> Vec<f32> {
    out.chunks(2).map(|frame| frame[0]).collect()
}

/// The positions `frames` frames of loop playback visit from `start`, one sample at a time:
/// the reference for the span iterator. With a loop `[ls, le)`, a position reaching `le` goes
/// back to `ls`; a start at or after `le` plays on.
fn model_positions(start: u64, frames: usize, looped: Option<(u64, u64)>) -> Vec<u64> {
    let mut position = start;
    (0..frames)
        .map(|_| {
            let current = position;
            position += 1;
            if let Some((ls, le)) = looped
                && position == le
            {
                position = ls;
            }
            current
        })
        .collect()
}

// --- loop, seek ---------------------------------------------------------------------------

#[test]
fn loop_playback_has_no_gap_or_duplicate_at_the_loop_point() {
    // (start, loop start, loop end): loops longer and shorter than the block, a boundary in the
    // middle of a block, a start inside, before and after the loop.
    let cases: [(u64, u64, u64); 8] = [
        (0, 1000, 3000),
        (1500, 1000, 3000),
        (4000, 1000, 3000),
        (0, 500, 503),
        (498, 500, 503),
        (96, 100, 164),
        (0, 0, 480),
        (0, 1, 2),
    ];
    const FRAMES: usize = 5_000;
    for workers in WORKERS {
        for (start, ls, le) in cases {
            let expected: Vec<f32> = model_positions(start, FRAMES, Some((ls, le)))
                .into_iter()
                .map(|p| p as f32)
                .collect();
            for block in BLOCKS {
                let mut renderer = renderer(Box::new(SampleProbe), workers);
                assert!(renderer.send(set_loop(ls, le)));
                assert!(renderer.send(transport(TransportCommand::Locate(start))));
                assert!(renderer.send(transport(TransportCommand::Play)));
                let got = left(&renderer.render(FRAMES, block));
                assert!(
                    got == expected,
                    "workers {workers}, loop {ls}..{le} from {start}, block {block}: first \
                     difference at frame {:?}",
                    got.iter().zip(&expected).position(|(a, b)| a != b)
                );
            }
        }
    }
}

#[test]
fn a_block_with_a_loop_wrap_is_cut_into_spans_that_tile_it() {
    // The probe asserts the tiling; here only that the wrapped span restarts at the loop start.
    let mut renderer = renderer(Box::new(SampleProbe), 0);
    renderer.send(set_loop(100, 164));
    renderer.send(transport(TransportCommand::Locate(150)));
    renderer.send(transport(TransportCommand::Play));
    let got = left(&renderer.render(40, 40));
    assert_eq!(got[0], 150.0);
    assert_eq!(got[13], 163.0);
    assert_eq!(got[14], 100.0, "the wrap lands on the loop start");
    assert_eq!(got[39], 125.0);
    assert_eq!(renderer.transport().position, 126);
}

#[test]
fn stopped_transport_plays_nothing_and_holds_its_position() {
    let mut renderer = renderer(Box::new(SampleProbe), 0);
    renderer.send(transport(TransportCommand::Locate(777)));
    assert!(left(&renderer.render(300, 100)).iter().all(|s| *s == -1.0));
    assert_eq!(renderer.transport().position, 777);
    assert!(!renderer.transport().playing);
    renderer.send(transport(TransportCommand::Play));
    assert_eq!(left(&renderer.render(3, 3)), [777.0, 778.0, 779.0]);
    renderer.send(transport(TransportCommand::Stop));
    assert!(left(&renderer.render(100, 50)).iter().all(|s| *s == -1.0));
    assert_eq!(renderer.transport().position, 780);
}

#[test]
fn seek_during_playback_lands_on_the_next_block_boundary_to_the_sample() {
    for workers in WORKERS {
        let mut renderer = renderer(Box::new(SampleProbe), workers);
        renderer.send(transport(TransportCommand::Play));
        assert_eq!(left(&renderer.render(100, 50))[99], 99.0);
        renderer.send(transport(TransportCommand::Locate(5_000)));
        let got = left(&renderer.render(100, 50));
        assert_eq!(got[0], 5_000.0);
        assert_eq!(got[99], 5_099.0);
        // Seek by beats: 2.5 beats at 120 bpm and 48 kHz.
        renderer.send(transport(TransportCommand::LocateBeats(2.5)));
        let got = left(&renderer.render(10, 10));
        assert_eq!(got[0], 60_000.0);
        // A seek into the loop, and one back out past its end.
        renderer.send(set_loop(1_000, 1_100));
        renderer.send(transport(TransportCommand::Locate(1_090)));
        let got = left(&renderer.render(20, 7));
        assert_eq!(got[9], 1_099.0);
        assert_eq!(got[10], 1_000.0);
        renderer.send(transport(TransportCommand::Locate(2_000)));
        let got = left(&renderer.render(5, 5));
        assert_eq!(got, [2_000.0, 2_001.0, 2_002.0, 2_003.0, 2_004.0]);
        assert_eq!(renderer.transport().position, 2_005);
    }
}

#[test]
fn seek_beats_follow_the_tempo_map() {
    // 60 bpm: one beat is one second.
    let timeline = Timeline::new(
        TempoMap::constant(60.0).unwrap(),
        TimeSignatureMap::default(),
    );
    let mut renderer = renderer(Box::new(SampleProbe), 0);
    renderer.link().swap_timeline(timeline).unwrap();
    renderer.send(transport(TransportCommand::LocateBeats(3.0)));
    renderer.send(transport(TransportCommand::Play));
    assert_eq!(left(&renderer.render(1, 1)), [144_000.0]);
}

// --- punch ----------------------------------------------------------------------------------

#[test]
fn punch_range_gates_recording_to_the_exact_frames() {
    // (loop, punch in, punch out) over 3000 frames from 0.
    let cases: [(LoopRange, u64, u64); 4] = [
        (None, 1_000, 1_500),
        (None, 0, 1),
        (Some((800, 1_200)), 1_000, 1_500),
        (Some((100, 163)), 120, 130),
    ];
    const FRAMES: usize = 3_000;
    for workers in WORKERS {
        for (looped, punch_in, punch_out) in cases {
            let expected: Vec<f32> = model_positions(0, FRAMES, looped)
                .into_iter()
                .map(|p| f32::from((punch_in..punch_out).contains(&p)))
                .collect();
            assert!(expected.contains(&1.0));
            for block in BLOCKS {
                let mut renderer = renderer(Box::new(RecordProbe), workers);
                if let Some((ls, le)) = looped {
                    renderer.send(set_loop(ls, le));
                }
                renderer.send(transport(TransportCommand::SetPunch {
                    enabled: true,
                    in_beats: beats(punch_in),
                    out_beats: beats(punch_out),
                }));
                renderer.send(transport(TransportCommand::SetRecordArm(true)));
                renderer.send(transport(TransportCommand::Play));
                let got = left(&renderer.render(FRAMES, block));
                assert!(
                    got == expected,
                    "workers {workers}, loop {looped:?}, punch {punch_in}..{punch_out}, block \
                     {block}: first difference at frame {:?}",
                    got.iter().zip(&expected).position(|(a, b)| a != b)
                );
            }
        }
    }
}

#[test]
fn recording_needs_arming_and_a_running_transport_and_ignores_the_punch_when_off() {
    let mut renderer = renderer(Box::new(RecordProbe), 0);
    renderer.send(transport(TransportCommand::Play));
    assert!(left(&renderer.render(100, 100)).iter().all(|s| *s == 0.0));
    assert!(!renderer.transport().recording);
    // Arm mid-playback: whole blocks record when no punch range is on.
    renderer.send(transport(TransportCommand::SetRecordArm(true)));
    assert!(left(&renderer.render(100, 30)).iter().all(|s| *s == 1.0));
    assert!(renderer.transport().recording);
    // A disabled punch range does not gate.
    renderer.send(transport(TransportCommand::SetPunch {
        enabled: false,
        in_beats: 1.0,
        out_beats: 2.0,
    }));
    assert!(left(&renderer.render(100, 30)).iter().all(|s| *s == 1.0));
    renderer.send(transport(TransportCommand::Stop));
    assert!(left(&renderer.render(100, 30)).iter().all(|s| *s == 0.0));
    assert!(!renderer.transport().recording);
}

// --- sample-accurate beat placement -----------------------------------------------------------

#[test]
fn beat_events_land_on_the_frame_the_tempo_map_says_across_blocks_and_loops() {
    // A ramp from 120 down to 60 bpm over 8 beats, then 60 bpm.
    let tempo = TempoMap::new(vec![
        TempoPoint::ramp(0.0, 120.0),
        TempoPoint::constant(8.0, 60.0),
    ])
    .unwrap();
    let timeline = Timeline::new(tempo.clone(), TimeSignatureMap::default());
    let beat_samples: Vec<u64> = (0..40)
        .map(|b| tempo.beats_to_samples(f64::from(b), RATE).round() as u64)
        .collect();
    let looped = Some((beat_samples[1] + 17, beat_samples[3] + 5));
    for looped in [None, looped] {
        let frames = 200_000;
        let expected: Vec<f32> = model_positions(0, frames, looped)
            .into_iter()
            .map(|p| f32::from(beat_samples.binary_search(&p).is_ok()))
            .collect();
        assert!(expected.iter().filter(|s| **s == 1.0).count() >= 4);
        for workers in WORKERS {
            for block in [64, 1000, 4096, 5000] {
                let mut renderer = renderer(Box::new(BeatProbe), workers);
                renderer.link().swap_timeline(timeline.clone()).unwrap();
                if let Some((ls, le)) = looped {
                    renderer.send(transport(TransportCommand::SetLoop {
                        enabled: true,
                        start_beats: tempo.samples_to_beats(ls as f64, RATE),
                        end_beats: tempo.samples_to_beats(le as f64, RATE),
                    }));
                }
                renderer.send(transport(TransportCommand::Play));
                let got = left(&renderer.render(frames, block));
                assert!(
                    got == expected,
                    "workers {workers}, loop {looped:?}, block {block}: first difference at \
                     frame {:?}",
                    got.iter().zip(&expected).position(|(a, b)| a != b)
                );
            }
        }
    }
}

// --- metronome and count-in -------------------------------------------------------------------

/// The click `MetronomeNode` should have produced at `onset`, written into `expected`.
fn add_click(expected: &mut [f32], onset: usize, accent: bool) {
    let len = MetronomeNode::click_len(RATE);
    let (level, hz) = if accent { (1.0, 1600.0) } else { (0.6, 1000.0) };
    for age in 0..len {
        let envelope = 1.0 - age as f64 / len as f64;
        let wave = (TAU * hz * age as f64 / f64::from(RATE)).cos();
        let sample = 0.5 * level * envelope * wave;
        if let Some(slot) = expected.get_mut(onset + age) {
            *slot = sample as f32;
        }
    }
}

fn assert_clicks(got: &[f32], clicks: &[(usize, bool)], what: &str) {
    let mut expected = vec![0.0_f32; got.len()];
    for &(onset, accent) in clicks {
        add_click(&mut expected, onset, accent);
    }
    for (frame, (a, b)) in got.iter().zip(&expected).enumerate() {
        assert!(
            (a - b).abs() < 1e-5,
            "{what}: frame {frame} is {a}, expected {b} (clicks at {clicks:?})"
        );
    }
}

#[test]
fn count_in_and_metronome_clicks_sit_on_the_exact_frames() {
    // 120 bpm in 4/4: a 1-bar count-in is 4 clicks 24000 samples apart (96000 samples), then
    // playback starts at beat 0 and clicks every beat.
    let count_in = [(0, true), (24_000, false), (48_000, false), (72_000, false)];
    let playback: Vec<(usize, bool)> = (0..5)
        .map(|beat| (96_000 + beat * 24_000, beat % 4 == 0))
        .collect();
    let frames = 96_000 + 4 * 24_000 + 1_000;
    let mut reference: Option<Vec<f32>> = None;
    for workers in WORKERS {
        for block in [64, 100, 480, 1000, 4096, 5000, 7_777] {
            let mut renderer = renderer(Box::new(MetronomeNode::new()), workers);
            renderer.send(transport(TransportCommand::SetMetronome {
                enabled: true,
                count_in_bars: 1,
            }));
            renderer.send(transport(TransportCommand::SetRecordArm(true)));
            renderer.send(transport(TransportCommand::Play));
            let got = left(&renderer.render(frames, block));
            let mut clicks = count_in.to_vec();
            clicks.extend(&playback);
            assert_clicks(&got, &clicks, &format!("workers {workers}, block {block}"));
            assert_eq!(renderer.transport().position, 4 * 24_000 + 1_000);
            // Bit-identical regardless of block size and worker count.
            match &reference {
                None => reference = Some(got),
                Some(first) => assert!(*first == got, "workers {workers}, block {block}"),
            }
        }
    }
}

#[test]
fn count_in_holds_the_position_and_only_runs_when_armed() {
    let mut renderer = renderer(Box::new(SampleProbe), 0);
    renderer.send(transport(TransportCommand::SetMetronome {
        enabled: false,
        count_in_bars: 2,
    }));
    renderer.send(transport(TransportCommand::Locate(5_000)));
    // Not armed: no count-in, playback starts at once.
    renderer.send(transport(TransportCommand::Play));
    assert_eq!(left(&renderer.render(2, 2)), [5_000.0, 5_001.0]);
    renderer.send(transport(TransportCommand::Stop));
    renderer.send(transport(TransportCommand::SetRecordArm(true)));
    renderer.send(transport(TransportCommand::Locate(5_000)));
    renderer.send(transport(TransportCommand::Play));
    // Armed: two bars (8 beats, 192000 samples) of count-in, position frozen, nothing plays.
    let count_in = 2 * 4 * 24_000;
    let mid = left(&renderer.render(count_in - 10, 4096));
    assert!(mid.iter().all(|s| *s == -1.0));
    let state = renderer.transport();
    assert!(state.counting_in && !state.playing && !state.recording);
    assert_eq!(state.position, 5_000);
    // The count-in ends mid-block; playback starts on the very next frame.
    let rest = left(&renderer.render(15, 15));
    assert_eq!(rest[9], -1.0);
    assert_eq!(rest[10], 5_000.0);
    assert_eq!(rest[14], 5_004.0);
    let state = renderer.transport();
    assert!(!state.counting_in && state.playing && state.recording);
    // Stop cancels a count-in.
    renderer.send(transport(TransportCommand::Stop));
    renderer.send(transport(TransportCommand::Play));
    renderer.render(10, 10);
    assert!(renderer.transport().counting_in);
    renderer.send(transport(TransportCommand::Stop));
    renderer.render(10, 10);
    assert!(!renderer.transport().counting_in);
}

#[test]
fn metronome_follows_time_signatures_and_tempo_ramps() {
    // 3/4 for two bars, then 6/8 (clicks every eighth note), tempo ramping 120 -> 90 over 6
    // beats and then holding.
    let tempo = TempoMap::new(vec![
        TempoPoint::ramp(0.0, 120.0),
        TempoPoint::constant(6.0, 90.0),
    ])
    .unwrap();
    let signatures = TimeSignatureMap::new(vec![
        (0, TimeSignature::new(3, 4).unwrap()),
        (2, TimeSignature::new(6, 8).unwrap()),
    ])
    .unwrap();
    // Beats of the clicks: 0, 1, 2, 3, 4, 5 (bars 0 and 1), then 6, 6.5, ... 8.5 (bar 2), 9...
    let mut grid: Vec<(f64, bool)> = (0..6).map(|b| (f64::from(b), b % 3 == 0)).collect();
    for bar in 0..3 {
        for eighth in 0..6 {
            grid.push((
                6.0 + f64::from(bar) * 3.0 + f64::from(eighth) * 0.5,
                eighth == 0,
            ));
        }
    }
    let frames = (tempo.beats_to_samples(grid.last().unwrap().0, RATE) as usize) + 5_000;
    let clicks: Vec<(usize, bool)> = grid
        .iter()
        .map(|&(beat, accent)| (tempo.beats_to_samples(beat, RATE).round() as usize, accent))
        .collect();
    let timeline = Timeline::new(tempo, signatures);
    for workers in WORKERS {
        for block in [333, 4096, 5000] {
            let mut renderer = renderer(Box::new(MetronomeNode::new()), workers);
            renderer.link().swap_timeline(timeline.clone()).unwrap();
            renderer.send(transport(TransportCommand::SetMetronome {
                enabled: true,
                count_in_bars: 0,
            }));
            renderer.send(transport(TransportCommand::Play));
            let got = left(&renderer.render(frames, block));
            assert_clicks(&got, &clicks, &format!("workers {workers}, block {block}"));
        }
    }
}

#[test]
fn metronome_is_silent_when_off_and_clicks_the_loop_start_but_not_the_loop_end() {
    let mut silent = renderer(Box::new(MetronomeNode::new()), 0);
    silent.send(transport(TransportCommand::Play));
    assert!(left(&silent.render(30_000, 1000)).iter().all(|s| *s == 0.0));
    // Loop beats 1..3 (24000..72000): clicks at the loop start each pass, none at the end.
    let mut renderer = renderer(Box::new(MetronomeNode::new()), 0);
    renderer.send(set_loop(24_000, 72_000));
    renderer.send(transport(TransportCommand::SetMetronome {
        enabled: true,
        count_in_bars: 0,
    }));
    renderer.send(transport(TransportCommand::Locate(24_000)));
    renderer.send(transport(TransportCommand::Play));
    let got = left(&renderer.render(48_000 * 2 + 1_000, 999));
    // Pass 1: beats 1 and 2 (the downbeat of bar 0 is not in the loop), then the wrap to beat 1.
    let clicks = [
        (0, false),
        (24_000, false),
        (48_000, false),
        (72_000, false),
        (96_000, false),
    ];
    assert_clicks(&got, &clicks, "looped metronome");
}

// --- timeline swaps ---------------------------------------------------------------------------

#[test]
fn timeline_swap_keeps_the_musical_position_and_moves_loop_bounds() {
    let mut renderer = renderer(Box::new(SampleProbe), 0);
    renderer.send(transport(TransportCommand::Play));
    // Two beats at 120 bpm.
    renderer.render(48_000, 4096);
    assert_eq!(renderer.transport().position, 48_000);
    let slow = Timeline::new(
        TempoMap::constant(60.0).unwrap(),
        TimeSignatureMap::default(),
    );
    renderer.link().swap_timeline(slow).unwrap();
    renderer.send(set_loop(0, 24_000));
    // Beat 2 at 60 bpm is sample 96000; the loop (beats 0..1) is now 48000 samples long, so
    // playback from 96000 is past it and just continues.
    let got = left(&renderer.render(4, 4));
    assert_eq!(got, [96_000.0, 96_001.0, 96_002.0, 96_003.0]);
    renderer.send(transport(TransportCommand::LocateBeats(0.5)));
    let got = left(&renderer.render(1, 1));
    assert_eq!(got, [24_000.0]);
    // Another swap, retired timelines come back to the control thread.
    let fast = Timeline::new(
        TempoMap::constant(240.0).unwrap(),
        TimeSignatureMap::default(),
    );
    renderer.link().swap_timeline(fast).unwrap();
    renderer.render(1, 1);
    renderer.link().collect_retired();
}

#[test]
fn default_timeline_is_120_bpm_in_4_4_and_the_position_is_mirrored_in_telemetry() {
    let (mut link, mut core) = CoreBuilder::new().build();
    link.send(transport(TransportCommand::LocateBeats(1.0)));
    link.send(transport(TransportCommand::Play));
    let mut buffer = vec![0.0_f32; 2 * 100];
    // No plan yet: the transport still moves.
    core.process(&mut buffer, 2, None);
    assert_eq!(core.transport().position, 24_100);
    assert_eq!(link.telemetry().transport_position(), 24_100);
}

// --- real-time safety ---------------------------------------------------------------------------

/// Worker count from `OW_TEST_WORKERS` (0 when unset), as in `rt_safety`.
#[cfg(debug_assertions)]
fn test_workers() -> usize {
    std::env::var("OW_TEST_WORKERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

/// Every transport feature at once under `assert_no_alloc`: playing, looping, punching,
/// metronome clicks, count-in, seeks, record arming and timeline swaps (with retired timelines
/// collected on the control side), on the callback thread and `OW_TEST_WORKERS` workers.
#[cfg(debug_assertions)]
#[test]
fn rt_safe_transport_traffic() {
    let mut graph = Graph::new();
    let metronome = graph.add_node(Box::new(MetronomeNode::new()));
    let samples = graph.add_node(Box::new(SampleProbe));
    let record = graph.add_node(Box::new(RecordProbe));
    let beat = graph.add_node(Box::new(BeatProbe));
    let master = graph.add_node(Box::new(MasterNode::new()));
    // Only nodes the master depends on run, so mix every probe into it.
    let sum = graph.add_node(Box::new(ow_engine::SumNode::new(4)));
    for (input, node) in [metronome, samples, record, beat].into_iter().enumerate() {
        graph.connect(node, 0, sum, input).unwrap();
    }
    graph.connect(sum, 0, master, 0).unwrap();
    graph.connect(sum, 0, master, 1).unwrap();
    graph.set_master(master).unwrap();

    let (mut link, mut core) = CoreBuilder::new().workers(test_workers()).build();
    link.swap_plan(graph.compile(RATE, MAX_BLOCK).unwrap())
        .unwrap();
    let mut buffer = vec![0.0_f32; 2 * 97];
    let ramp = |bpm: f64| {
        Timeline::new(
            TempoMap::new(vec![
                TempoPoint::ramp(0.0, 90.0),
                TempoPoint::constant(4.0, bpm),
            ])
            .unwrap(),
            TimeSignatureMap::new(vec![
                (0, TimeSignature::new(7, 8).unwrap()),
                (3, TimeSignature::COMMON),
            ])
            .unwrap(),
        )
    };
    let commands = |step: u64| -> Message {
        let b = (step % 13) as f64 * 0.01;
        match step % 11 {
            0 => transport(TransportCommand::Play),
            1 => set_loop(300 + step % 7, 330 + step % 13),
            2 => transport(TransportCommand::SetPunch {
                enabled: step.is_multiple_of(2),
                in_beats: b,
                out_beats: b + 0.01,
            }),
            3 => transport(TransportCommand::SetMetronome {
                enabled: true,
                count_in_bars: (step % 3) as u32,
            }),
            4 => transport(TransportCommand::SetRecordArm(step.is_multiple_of(2))),
            5 => transport(TransportCommand::LocateBeats(b)),
            6 => transport(TransportCommand::Locate(step % 500)),
            7 => transport(TransportCommand::Stop),
            8 => transport(TransportCommand::SetLoop {
                enabled: false,
                start_beats: 0.0,
                end_beats: 0.0,
            }),
            9 => transport(TransportCommand::SetRecordArm(true)),
            _ => transport(TransportCommand::Play),
        }
    };
    for step in 0..4_000_u64 {
        link.send(commands(step));
        if step % 37 == 0 {
            let _ = link.swap_timeline(ramp(60.0 + (step % 90) as f64));
        }
        if step % 50 == 0 {
            link.collect_retired();
        }
        // The cycle runs under `assert_no_alloc`: any allocation aborts the test binary.
        core.process(&mut buffer, 2, None);
    }
    assert_eq!(link.telemetry().cycles(), 4_000);
    assert_eq!(link.telemetry().plan_swaps(), 1);
    assert_eq!(link.message_overflow(), 0);
    assert_eq!(link.telemetry().unrouted(), 0);
}
