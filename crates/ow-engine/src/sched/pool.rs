//! The DSP worker pool: threads that help the callback thread run each cycle's nodes.
//!
//! See the [module overview](super) for the wake/park protocol and the plan hazard pointers.

// Workers dereference the published plan pointer under a hazard pointer.
#![allow(unsafe_code)]

use std::hint::spin_loop;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

use assert_no_alloc::assert_no_alloc;

use super::AbortOnPanic;
use super::denormal::DenormalGuard;
use crate::graph::Telemetry;
use crate::plan::PlanData;
use crate::workgroup::AudioWorkgroup;

/// How long an idle worker spins on the wake generation before it parks.
pub(crate) const SPIN: Duration = Duration::from_micros(50);
/// Spin iterations between clock reads while spinning.
const SPINS_PER_CLOCK_READ: u32 = 64;

/// Per-worker shared state, on its own cache lines.
#[repr(align(128))]
struct WorkerSlot {
    /// The plan this worker may be dereferencing, or null.
    hazard: AtomicPtr<PlanData>,
    /// Set just before the worker parks.
    sleeping: AtomicBool,
}

/// State shared by the audio thread, the workers and the control thread.
pub(crate) struct Shared {
    /// The plan workers should help with (the audio thread's current plan), or null.
    job: AtomicPtr<PlanData>,
    /// Bumped once per cycle (and on shutdown) to wake workers.
    generation: AtomicU64,
    shutdown: AtomicBool,
    slots: Box<[WorkerSlot]>,
}

impl Shared {
    /// Whether any worker may still be reading `plan`. A plan that is no longer published (it
    /// was swapped out) and is not in use can be freed: a worker re-checks the published
    /// pointer after setting its hazard, so it will never start reading it again.
    pub(crate) fn in_use(&self, plan: *const PlanData) -> bool {
        self.slots
            .iter()
            .any(|slot| ptr::eq(slot.hazard.load(Ordering::SeqCst), plan))
    }
}

/// A real-time priority grant, held by the worker for its lifetime.
pub(crate) struct PriorityHandle {
    _handle: Option<audio_thread_priority::RtPriorityHandle>,
}

/// Promotes the calling thread for `(block frames, sample rate)`; `None` on failure.
pub(crate) type Promote = fn(u32, u32) -> Option<PriorityHandle>;

/// The real promotion, through `audio_thread_priority`.
fn promote_with_audio_thread_priority(frames: u32, rate: u32) -> Option<PriorityHandle> {
    audio_thread_priority::promote_current_thread_to_real_time(frames, rate)
        .ok()
        .map(|handle| PriorityHandle {
            _handle: Some(handle),
        })
}

/// A promotion that always succeeds without changing the thread (tests).
#[cfg(test)]
pub(crate) fn promote_noop(_: u32, _: u32) -> Option<PriorityHandle> {
    Some(PriorityHandle { _handle: None })
}

/// A promotion that always fails (tests).
#[cfg(test)]
pub(crate) fn promote_never(_: u32, _: u32) -> Option<PriorityHandle> {
    None
}

/// How to start the workers.
#[derive(Clone)]
pub(crate) struct PoolConfig {
    /// Number of worker threads (the callback thread is not counted).
    pub(crate) workers: usize,
    /// `(block frames, sample rate)` for `audio_thread_priority`, or `None` to keep the normal
    /// priority (offline rendering, tests).
    pub(crate) realtime: Option<(u32, u32)>,
    /// macOS audio workgroup every worker joins.
    pub(crate) workgroup: Option<Arc<AudioWorkgroup>>,
    /// How workers promote themselves when `realtime` is set (replaceable in tests).
    pub(crate) promote: Promote,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            workers: 0,
            realtime: None,
            workgroup: None,
            promote: promote_with_audio_thread_priority,
        }
    }
}

/// Owns the worker threads; dropping it stops and joins them.
pub(crate) struct Pool {
    shared: Arc<Shared>,
    threads: Vec<Thread>,
    handles: Vec<JoinHandle<()>>,
}

impl Pool {
    /// Starts `config.workers` threads (fewer if the OS refuses to create more).
    pub(crate) fn spawn(config: &PoolConfig, telemetry: &Arc<Telemetry>) -> Self {
        let shared = Arc::new(Shared {
            job: AtomicPtr::new(ptr::null_mut()),
            generation: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
            slots: (0..config.workers)
                .map(|_| WorkerSlot {
                    hazard: AtomicPtr::new(ptr::null_mut()),
                    sleeping: AtomicBool::new(false),
                })
                .collect(),
        });
        let mut threads = Vec::with_capacity(config.workers);
        let mut handles = Vec::with_capacity(config.workers);
        for index in 0..config.workers {
            let worker_shared = Arc::clone(&shared);
            let worker_telemetry = Arc::clone(telemetry);
            let realtime = config.realtime;
            let promote = config.promote;
            let workgroup = config.workgroup.clone();
            let spawned = thread::Builder::new()
                .name(format!("ow-dsp-{}", index + 1))
                .spawn(move || {
                    worker_main(
                        &worker_shared,
                        index,
                        &worker_telemetry,
                        realtime,
                        promote,
                        workgroup.as_deref(),
                    );
                });
            match spawned {
                Ok(handle) => {
                    threads.push(handle.thread().clone());
                    handles.push(handle);
                }
                Err(_) => break,
            }
        }
        Self {
            shared,
            threads,
            handles,
        }
    }

    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Number of running worker threads.
    pub(crate) fn workers(&self) -> usize {
        self.threads.len()
    }

    /// Publishes the plan workers should help with. Audio thread only.
    pub(crate) fn publish(&self, plan: *mut PlanData) {
        self.shared.job.store(plan, Ordering::SeqCst);
    }

    /// Tells the workers a cycle has begun. Audio thread only; never blocks.
    pub(crate) fn wake(&self) {
        if self.threads.is_empty() {
            return;
        }
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        for (slot, thread) in self.shared.slots.iter().zip(&self.threads) {
            if slot.sleeping.load(Ordering::SeqCst) {
                thread.unpark();
            }
        }
    }

    /// Stops and joins the workers; idempotent.
    pub(crate) fn shutdown(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        for thread in &self.threads {
            thread.unpark();
        }
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
        self.threads.clear();
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_main(
    shared: &Shared,
    index: usize,
    telemetry: &Telemetry,
    realtime: Option<(u32, u32)>,
    promote: Promote,
    workgroup: Option<&AudioWorkgroup>,
) {
    // A panic anywhere below (in practice: in a node) aborts instead of stalling the cycle.
    let _abort = AbortOnPanic;
    let _denormals = DenormalGuard::new();
    // A worker that cannot get real-time priority would make real-time cycles wait on a
    // normal-priority thread whenever it claims a node, so it does not take part at all.
    let _priority = match realtime {
        Some((frames, rate)) => match promote(frames, rate) {
            Some(handle) => {
                telemetry.count_realtime_worker();
                Some(handle)
            }
            None => {
                telemetry.count_unpromoted_worker();
                return;
            }
        },
        None => None,
    };
    let membership = workgroup.and_then(|workgroup| workgroup.join().ok());
    if membership.is_some() {
        telemetry.count_workgroup_worker();
    }
    // Everything a worker does from here on runs under the allocation guard (active in debug
    // and test builds that install `AllocDisabler`).
    assert_no_alloc(|| run(shared, index, telemetry));
    // `membership` (leaves the workgroup) and `priority` end here, on this thread.
}

fn run(shared: &Shared, index: usize, telemetry: &Telemetry) {
    let slot = &shared.slots[index];
    let mut seen = shared.generation.load(Ordering::Acquire);
    while wait_for_work(shared, slot, &mut seen) {
        let plan = shared.job.load(Ordering::SeqCst);
        if plan.is_null() {
            continue;
        }
        slot.hazard.store(plan, Ordering::SeqCst);
        if ptr::eq(shared.job.load(Ordering::SeqCst), plan) {
            // SAFETY: the plan was still published after our hazard became visible, so the
            // control thread's `in_use` check sees the hazard and does not free the plan until
            // we clear it below; the audio thread never frees plans.
            let ran = unsafe { &*plan }.work(|| shared.shutdown.load(Ordering::Relaxed));
            if ran > 0 {
                telemetry.count_nodes(index + 1, ran);
            }
        }
        slot.hazard.store(ptr::null_mut(), Ordering::Release);
    }
}

/// Waits for the next wake generation: spins for [`SPIN`], then parks. Returns false on
/// shutdown.
fn wait_for_work(shared: &Shared, slot: &WorkerSlot, seen: &mut u64) -> bool {
    let mut spins: u32 = 0;
    let mut spin_start: Option<Instant> = None;
    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            return false;
        }
        let generation = shared.generation.load(Ordering::Acquire);
        if generation != *seen {
            *seen = generation;
            return true;
        }
        spins = spins.wrapping_add(1);
        if !spins.is_multiple_of(SPINS_PER_CLOCK_READ)
            || spin_start.get_or_insert_with(Instant::now).elapsed() < SPIN
        {
            spin_loop();
            continue;
        }
        // Announce sleep, then re-check: the waker bumps the generation before it reads the
        // flag, so either we see the new generation here or it sees the flag and unparks us.
        slot.sleeping.store(true, Ordering::SeqCst);
        if shared.generation.load(Ordering::SeqCst) == *seen
            && !shared.shutdown.load(Ordering::SeqCst)
        {
            thread::park();
        }
        slot.sleeping.store(false, Ordering::SeqCst);
    }
}
