//! The audio thread's writes to the [`TelemetryBank`] never allocate: every call runs under
//! `assert_no_alloc`, which aborts the process on any allocation, reallocation or free.
//! (`assert_no_alloc` is compiled out of release builds, so this file only exists in debug.)

#![cfg(debug_assertions)]

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use ow_ipc::{METER_SLOTS, Overflow, TelemetryBank};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn telemetry_rt_bank_writes_do_not_allocate() {
    let bank = TelemetryBank::new();
    assert_no_alloc(|| {
        for round in 0..10_000_u32 {
            let level = (round % 100) as f32 / 100.0;
            for slot in 0..METER_SLOTS + 4 {
                bank.write_meter(slot, [level, level], [level * 0.5, level * 0.5]);
            }
            bank.set_position(u64::from(round) * 480);
            bank.set_cpu_load(level);
            bank.set_overflow(Overflow::Ring, u64::from(round));
            bank.set_overflow(Overflow::Unrouted, 1);
        }
    });
    assert!(bank.take_meter(7).unwrap().0[0] > 0.0);
}
