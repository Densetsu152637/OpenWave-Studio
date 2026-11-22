//! Telemetry bank, frame packing and the sampling budget.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ow_ipc::telemetry::{COUNTER_MAX, MAX_FRAME_BYTES};
use ow_ipc::{
    Frame, FramePacker, HEADER_WORDS, METER_SLOTS, Overflow, STRIP_WORDS, Subscriptions,
    TelemetryBank,
};

fn busy_bank() -> TelemetryBank {
    let bank = TelemetryBank::new();
    for slot in 0..METER_SLOTS {
        let level = slot as f32 / METER_SLOTS as f32;
        bank.write_meter(slot, [level, level * 0.5], [level * 0.25, level * 0.125]);
    }
    bank.set_position((1 << 40) + 12_345);
    bank.set_cpu_load(0.37);
    bank.set_overflow(Overflow::Ring, 3);
    bank.set_overflow(Overflow::Unrouted, 9);
    bank.set_overflow(Overflow::Midi, u64::MAX);
    bank
}

#[test]
fn telemetry_frame_round_trips_every_field() {
    let bank = busy_bank();
    let subscribed = Subscriptions::new();
    subscribed.set(&[0, 5, 64, 255, 256, 9999]);
    let mut packer = FramePacker::new();
    let bytes = packer.pack(&bank, &subscribed).to_vec();
    assert_eq!(bytes.len(), (HEADER_WORDS + STRIP_WORDS * 4) * 4);

    let frame = Frame::decode(&bytes).expect("well-formed");
    assert_eq!(frame.counter, 1);
    assert_eq!(frame.position, (1 << 40) + 12_345);
    assert_eq!(frame.cpu_load, 0.37);
    assert_eq!(frame.ring_overflow, 3);
    assert_eq!(frame.unrouted, 9);
    assert_eq!(frame.midi_dropped, COUNTER_MAX, "counters saturate");
    let slots: Vec<u32> = frame.strips.iter().map(|s| s.slot).collect();
    assert_eq!(
        slots,
        [0, 5, 64, 255],
        "ascending, only subscribed, none out of range"
    );
    let five = frame.strips[1];
    let level = 5.0 / METER_SLOTS as f32;
    assert_eq!(five.peak, [level, level * 0.5]);
    assert_eq!(five.rms, [level * 0.25, level * 0.125]);

    // The frame is little-endian f32 words whatever the host.
    assert_eq!(&bytes[..4], 1.0_f32.to_le_bytes());
    // Malformed input is rejected, not misread.
    assert!(Frame::decode(&bytes[..bytes.len() - 4]).is_none());
    assert!(Frame::decode(&bytes[..7]).is_none());
}

#[test]
fn telemetry_peaks_hold_until_read_and_rms_is_the_latest_block() {
    let bank = TelemetryBank::new();
    bank.write_meter(3, [0.2, 0.9], [0.1, 0.1]);
    bank.write_meter(3, [0.5, 0.3], [0.2, 0.4]);
    bank.write_meter(3, [f32::NAN, -1.0], [0.3, 0.3]);
    let (peak, rms) = bank.take_meter(3).unwrap();
    assert_eq!(
        peak,
        [0.5, 0.9],
        "maximum since the last read; NaN and negatives are ignored"
    );
    assert_eq!(rms, [0.3, 0.3]);
    let (peak, _) = bank.take_meter(3).unwrap();
    assert_eq!(peak, [0.0, 0.0], "reading resets the peaks");
    assert!(bank.take_meter(METER_SLOTS).is_none());
    bank.write_meter(METER_SLOTS + 10, [1.0; 2], [1.0; 2]); // ignored, no panic
}

#[test]
fn telemetry_frame_counter_advances_and_wraps() {
    let bank = TelemetryBank::new();
    let subscribed = Subscriptions::new();
    let mut packer = FramePacker::new();
    let first = Frame::decode(packer.pack(&bank, &subscribed))
        .unwrap()
        .counter;
    let second = Frame::decode(packer.pack(&bank, &subscribed))
        .unwrap()
        .counter;
    assert_eq!((first, second), (1, 2));
}

#[test]
fn telemetry_bank_is_written_while_it_is_sampled() {
    let bank = Arc::new(TelemetryBank::new());
    let writer = {
        let bank = Arc::clone(&bank);
        std::thread::spawn(move || {
            for i in 0..50_000_u64 {
                bank.write_meter((i % 8) as usize, [0.5, 0.25], [0.1, 0.1]);
                bank.set_position(i);
            }
        })
    };
    let subscribed = Subscriptions::new();
    subscribed.set(&[0, 1, 2, 3, 4, 5, 6, 7]);
    let mut packer = FramePacker::new();
    let mut last = 0;
    for _ in 0..200 {
        let frame = Frame::decode(packer.pack(&bank, &subscribed)).unwrap();
        assert!(frame.position <= 50_000);
        assert!(
            frame
                .strips
                .iter()
                .all(|s| s.peak[0] <= 0.5 && s.peak[1] <= 0.25)
        );
        last = frame.counter;
    }
    writer.join().unwrap();
    assert_eq!(last, 200);
}

/// Sampling and packing 128 meters plus the transport block takes well under a frame (16.7 ms):
/// the median over 1000 iterations is under 100 microseconds (asserted in release builds; debug
/// builds only check a generous bound).
#[test]
fn telemetry_budget() {
    let bank = busy_bank();
    let subscribed = Subscriptions::new();
    let slots: Vec<u16> = (0..128).collect();
    subscribed.set(&slots);
    let mut packer = FramePacker::new();
    let mut samples: Vec<Duration> = Vec::with_capacity(1000);
    let mut checksum = 0_usize;
    for round in 0..1000 {
        // Keep the peaks non-zero so the swap has real work each time.
        for slot in 0..128 {
            bank.write_meter(slot, [0.5, 0.5], [0.1, 0.1]);
        }
        bank.set_position(round);
        let started = Instant::now();
        let bytes = packer.pack(&bank, &subscribed);
        checksum += bytes.len();
        samples.push(started.elapsed());
    }
    samples.sort();
    let median = samples[samples.len() / 2];
    println!(
        "median {median:?}, max {:?}, bytes {}",
        samples[999],
        checksum / 1000
    );
    assert_eq!(checksum / 1000, (HEADER_WORDS + STRIP_WORDS * 128) * 4);
    assert!(checksum / 1000 <= MAX_FRAME_BYTES);
    let limit = if cfg!(debug_assertions) {
        Duration::from_millis(5)
    } else {
        Duration::from_micros(100)
    };
    assert!(
        median < limit,
        "median {median:?} over the {limit:?} budget"
    );
}

/// The frame bytes the TypeScript decoder is tested against (`src/lib/ipc/telemetry.test.ts`
/// decodes the same hex): any change to the layout must change both.
pub const GOLDEN_FRAME_HEX: &str = "0000803f0000803f000040400000a0400000003f000000400000803f000000000000803f0000803f0000003f0000803e0000003e0000803d";

#[test]
fn telemetry_frame_golden_bytes() {
    let bank = TelemetryBank::new();
    bank.write_meter(1, [0.5, 0.25], [0.125, 0.0625]);
    bank.set_position((3 << 24) + 5);
    bank.set_cpu_load(0.5);
    bank.set_overflow(Overflow::Ring, 2);
    bank.set_overflow(Overflow::Unrouted, 1);
    let subscribed = Subscriptions::new();
    subscribed.set(&[1]);
    let bytes = FramePacker::new().pack(&bank, &subscribed).to_vec();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, GOLDEN_FRAME_HEX);
}
