//! Track-count benchmark (architecture section 12): 200 tracks, each a synth and three
//! effects, at 128-frame blocks and 48 kHz. One iteration is one engine cycle, so the time per
//! iteration compares directly with the 2.667 ms callback budget (target: under 70% on an
//! 8-core desktop). Tracked in CI, not gated.
//!
//! Each track is one coarse node, as the architecture prescribes for track chains: a stand-in
//! synth (two detuned sine oscillators) followed by three biquad filters. Run with
//! `cargo bench -p ow-engine --bench tracks` (add `-- --quick` for a short run).

use std::f32::consts::TAU;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use ow_engine::graph::nodes::MasterNode;
use ow_engine::{CoreBuilder, Graph, Node, ProcessContext, RenderPlan, default_worker_count};

const TRACKS: usize = 200;
const BLOCK: usize = 128;
const SAMPLE_RATE: u32 = 48_000;

/// Direct-form-I biquad (RBJ low-pass).
#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn low_pass(cutoff: f32, q: f32) -> Self {
        let w = TAU * cutoff / SAMPLE_RATE as f32;
        let alpha = w.sin() / (2.0 * q);
        let cos = w.cos();
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cos) / 2.0 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// One track: synth plus three effects, as a single coarse node.
struct Track {
    phases: [f32; 2],
    steps: [f32; 2],
    effects: [Biquad; 3],
}

impl Track {
    fn new(index: usize) -> Self {
        let freq = 55.0 * (1.0 + (index % 24) as f32 / 12.0);
        let rate = SAMPLE_RATE as f32;
        Self {
            phases: [0.0, 0.25],
            steps: [freq / rate, freq * 1.003 / rate],
            effects: [
                Biquad::low_pass(4_000.0, 0.7),
                Biquad::low_pass(2_500.0, 1.2),
                Biquad::low_pass(8_000.0, 0.5),
            ],
        }
    }
}

impl Node for Track {
    fn name(&self) -> &'static str {
        "track"
    }
    fn input_channels(&self) -> usize {
        0
    }
    fn output_channels(&self) -> usize {
        1
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        for sample in ctx.outputs.channel_mut(0) {
            let mut synth = 0.0;
            for (phase, step) in self.phases.iter_mut().zip(self.steps) {
                synth += (*phase * TAU).sin() * 0.05;
                *phase += step;
                if *phase >= 1.0 {
                    *phase -= 1.0;
                }
            }
            let mut value = synth;
            for effect in &mut self.effects {
                value = effect.run(value);
            }
            *sample = value;
        }
    }
}

fn session() -> RenderPlan {
    let mut graph = Graph::new();
    let master = graph.add_node(Box::new(MasterNode::new()));
    for index in 0..TRACKS {
        let track = graph.add_node(Box::new(Track::new(index)));
        graph.connect(track, 0, master, 0).unwrap();
        graph.connect(track, 0, master, 1).unwrap();
    }
    graph.set_master(master).unwrap();
    graph.compile(SAMPLE_RATE, BLOCK).unwrap()
}

fn tracks(c: &mut Criterion) {
    let mut group = c.benchmark_group(format!("tracks_{TRACKS}x4_block{BLOCK}"));
    let mut counts = vec![0, default_worker_count()];
    counts.dedup();
    for workers in counts {
        let (mut link, mut core) = CoreBuilder::new().workers(workers).build();
        link.swap_plan(session()).expect("fresh plan queue");
        let mut buffer = vec![0.0_f32; BLOCK * 2];
        core.process(&mut buffer, 2, None);
        group.bench_function(format!("workers_{workers}"), |b| {
            b.iter(|| {
                core.process(black_box(&mut buffer), 2, None);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, tracks);
criterion_main!(benches);
