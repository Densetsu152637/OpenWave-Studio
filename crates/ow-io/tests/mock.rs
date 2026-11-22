//! The mock backend as the realtime path: a real engine plan driven through the `AudioBackend`
//! callback with no allocation, and MIDI from two producer threads arriving without loss.
//!
//! `AllocDisabler` is this binary's global allocator and `AudioCore::process` runs every cycle
//! under `assert_no_alloc`, so any allocation in the engine cycle aborts the test. The mock
//! harness itself sizes its buffers at open and never allocates per callback.

#[cfg(not(debug_assertions))]
compile_error!(
    "these tests need debug assertions: assert_no_alloc is compiled out of release builds, so \
     they would pass without checking anything. Run them in the dev or test profile."
);

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use assert_no_alloc::AllocDisabler;
use ow_engine::graph::nodes::{GainNode, MasterNode, OscillatorNode};
use ow_engine::{
    ControlLink, CoreBuilder, Graph, MAX_BLOCK, Message, MidiMessage, Node, NodeId, ProcessContext,
};
use ow_io::mock::{MockBackend, MockConfig, MockMidiBackend};
use ow_io::{
    AudioBackend, AudioCallback, AudioSelection, AudioStream, BackendRegistry, MidiBackend,
    StreamInfo, StreamRequest,
};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const SAMPLE_RATE: u32 = 48_000;
const BUFFER: u32 = 64;
const CALLBACKS: usize = 10_000;
const EVENTS_PER_PRODUCER: u32 = 5_000;
const PRODUCERS: usize = 2;

/// Counts MIDI per producer and checks each producer's sequence arrives in order and complete.
/// Status is `0x90 | producer`; data1/data2 carry the 14-bit sequence number.
#[derive(Default)]
struct MidiTally {
    received: [AtomicU64; PRODUCERS],
    next_seq: [AtomicU32; PRODUCERS],
    out_of_order: AtomicU64,
}

struct MidiSink {
    tally: Arc<MidiTally>,
}

impl Node for MidiSink {
    fn name(&self) -> &'static str {
        "midi-sink"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        ctx.outputs.channel_mut(0).fill(0.0);
    }
    fn on_midi(&mut self, message: MidiMessage) {
        let producer = usize::from(message.status & 0x0F);
        let sequence = u32::from(message.data1) | (u32::from(message.data2) << 7);
        if producer >= PRODUCERS {
            self.tally.out_of_order.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let expected = self.tally.next_seq[producer].load(Ordering::Relaxed);
        if sequence != expected % 16_384 {
            self.tally.out_of_order.fetch_add(1, Ordering::Relaxed);
        }
        self.tally.next_seq[producer].store(expected + 1, Ordering::Relaxed);
        self.tally.received[producer].fetch_add(1, Ordering::Relaxed);
    }
}

struct Rig {
    stream: Box<dyn AudioStream>,
    info: StreamInfo,
    link: ControlLink,
    senders: Vec<ow_engine::RingSender<Message>>,
    sink: NodeId,
    tally: Arc<MidiTally>,
}

/// Opens the mock device through the registry, building the engine core inside the factory the
/// way `ow-engine` does on a real stream.
fn open_rig(backend: &MockBackend, producers: usize) -> Rig {
    let registry = BackendRegistry::with(vec![Arc::new(backend.clone()) as Arc<dyn AudioBackend>]);
    let tally = Arc::new(MidiTally::default());
    let (tx, rx) = mpsc::channel();
    let factory_tally = Arc::clone(&tally);
    let stream = registry
        .open(
            &AudioSelection {
                backend: "mock".into(),
                request: StreamRequest::default(),
            },
            Box::new(move |info: &StreamInfo| -> AudioCallback {
                let mut builder = CoreBuilder::new().workers(0);
                let senders: Vec<_> = (0..producers)
                    .map(|_| builder.producer_ring(1 << 14))
                    .collect();
                let (mut link, mut core) = builder.build();
                let mut graph = Graph::new();
                let osc = graph.add_node(Box::new(OscillatorNode::new(440.0, 0.25)));
                let gain = graph.add_node(Box::new(GainNode::new(1, 0.5)));
                let sink = graph.add_node(Box::new(MidiSink {
                    tally: factory_tally,
                }));
                let master = graph.add_node(Box::new(MasterNode::new()));
                graph.connect(osc, 0, gain, 0).unwrap();
                graph.connect(gain, 0, master, 0).unwrap();
                graph.connect(sink, 0, master, 1).unwrap();
                graph.set_master(master).unwrap();
                link.swap_plan(graph.compile(info.sample_rate, MAX_BLOCK).unwrap())
                    .unwrap();
                tx.send((link, senders, sink)).unwrap();
                let channels = usize::from(info.output_channels);
                Box::new(move |out: &mut [f32], input: Option<&[f32]>| {
                    core.process(out, channels, input);
                })
            }),
        )
        .expect("the mock device opens");
    let (link, senders, sink) = rx.recv().unwrap();
    let info = stream.info().clone();
    Rig {
        stream,
        info,
        link,
        senders,
        sink,
        tally,
    }
}

fn config() -> MockConfig {
    MockConfig {
        sample_rate: SAMPLE_RATE,
        buffer_frames: BUFFER,
        capture_frames: CALLBACKS * BUFFER as usize,
        ..MockConfig::default()
    }
}

#[test]
fn mock_drives_a_plan_through_10000_callbacks_without_allocating() {
    let backend = MockBackend::new(config());
    let handle = backend.handle();
    let mut rig = open_rig(&backend, 0);
    assert_eq!(rig.info.buffer_frames, Some(BUFFER));
    assert_eq!(rig.info.sample_rate, SAMPLE_RATE);
    rig.stream.start().unwrap();
    handle.run_callbacks(CALLBACKS).unwrap();
    assert_eq!(handle.callbacks_run(), CALLBACKS as u64);

    // The tone really came through: 440 Hz at 0.125 peak (0.25 * 0.5 gain).
    let captured = handle.captured();
    assert_eq!(captured.len(), CALLBACKS * BUFFER as usize * 2);
    let peak = captured.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!((peak - 0.125).abs() < 0.005, "peak = {peak}");
    // Left carries the tone; right carries the (silent) MIDI sink.
    assert!(captured.chunks(2).all(|frame| frame[1] == 0.0));
    let crossings = captured
        .chunks(2)
        .map(|frame| frame[0])
        .collect::<Vec<_>>()
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    let seconds = (CALLBACKS * BUFFER as usize) as f64 / f64::from(SAMPLE_RATE);
    let expected = (440.0 * seconds) as usize;
    assert!(
        crossings.abs_diff(expected) <= 2,
        "{crossings} crossings, expected about {expected}"
    );
    assert_eq!(rig.link.telemetry().cycles(), CALLBACKS as u64);
    drop(rig.stream);
}

#[test]
fn mock_midi_from_two_producer_threads_arrives_without_loss() {
    let backend = MockBackend::new(config());
    let handle = backend.handle();
    let mut rig = open_rig(&backend, PRODUCERS);
    let midi = MockMidiBackend::new(["Port A", "Port B"]);
    const PORTS: [&str; 2] = ["Port A", "Port B"];
    let timestamps: Arc<Vec<Mutex<Vec<u64>>>> =
        Arc::new((0..PRODUCERS).map(|_| Mutex::new(Vec::new())).collect());
    let overflow = Arc::new(AtomicU64::new(0));

    // One ring per MIDI callback thread: each connection's handler owns its sender.
    let sink = rig.sink;
    let mut connections = Vec::new();
    for (index, port) in PORTS.iter().enumerate() {
        let mut sender = rig.senders.remove(0);
        let stamps = Arc::clone(&timestamps);
        let overflow = Arc::clone(&overflow);
        connections.push(
            midi.connect_input(
                port,
                index as u16,
                Box::new(move |event| {
                    stamps[index].lock().unwrap().push(event.timestamp_us);
                    let pushed = sender.push(Message::Midi {
                        node: sink,
                        message: MidiMessage {
                            status: event.status(),
                            data1: event.data1(),
                            data2: event.data2(),
                        },
                        offset: 0,
                    });
                    if !pushed {
                        overflow.fetch_add(1, Ordering::Relaxed);
                    }
                }),
            )
            .unwrap(),
        );
    }

    rig.stream.start().unwrap();
    let producers: Vec<_> = PORTS
        .iter()
        .enumerate()
        .map(|(index, port)| {
            let midi = midi.clone();
            thread::spawn(move || {
                for sequence in 0..EVENTS_PER_PRODUCER {
                    let bytes = [
                        0x90 | index as u8,
                        (sequence & 0x7F) as u8,
                        ((sequence >> 7) & 0x7F) as u8,
                    ];
                    // Strictly increasing synthetic timestamps, 10 microseconds apart.
                    assert!(midi.send(port, u64::from(sequence) * 10, &bytes));
                }
                midi.flush(port);
            })
        })
        .collect();

    // Run callbacks while the producers feed their rings.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut ran = 0;
    while ran < CALLBACKS || producers.iter().any(|p| !p.is_finished()) {
        assert!(Instant::now() < deadline, "timed out after {ran} callbacks");
        handle.run_callbacks(10).unwrap();
        ran += 10;
        if ran % 1000 == 0 {
            thread::yield_now();
        }
    }
    for producer in producers {
        producer.join().unwrap();
    }
    // Drain whatever was still queued when the producers finished.
    handle.run_callbacks(8).unwrap();

    assert_eq!(overflow.load(Ordering::Relaxed), 0, "no ring overflowed");
    assert_eq!(
        rig.link.telemetry().unrouted(),
        0,
        "every message found its node"
    );
    // The engine's node saw every event, per producer, in order.
    for index in 0..PRODUCERS {
        assert_eq!(
            rig.tally.received[index].load(Ordering::Relaxed),
            u64::from(EVENTS_PER_PRODUCER),
            "producer {index} events reached the node"
        );
    }
    assert_eq!(rig.tally.out_of_order.load(Ordering::Relaxed), 0);
    for (index, stamps) in timestamps.iter().enumerate() {
        let stamps = stamps.lock().unwrap();
        assert_eq!(
            stamps.len(),
            EVENTS_PER_PRODUCER as usize,
            "producer {index}"
        );
        assert!(
            stamps.windows(2).all(|w| w[0] < w[1]),
            "timestamps of producer {index} stay ordered"
        );
        assert_eq!(stamps[0], 0);
        assert_eq!(
            *stamps.last().unwrap(),
            u64::from(EVENTS_PER_PRODUCER - 1) * 10
        );
    }
    drop(connections);
    drop(rig.stream);
}
