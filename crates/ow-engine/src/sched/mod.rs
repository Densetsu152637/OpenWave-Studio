//! Parallel DAG scheduler: per-node dependency counters, a lock-free ready queue, and a pool of
//! real-time DSP worker threads that help the audio callback thread run each cycle.
//!
//! # One cycle
//!
//! 1. The callback thread resets the plan's dependency counters and publishes the nodes without
//!    predecessors ([`dag::Dag::begin`]).
//! 2. It bumps the pool's wake generation and unparks any worker that went to sleep
//!    ([`pool::Pool::wake`]).
//! 3. Every participating thread, the callback thread included, claims ready nodes, runs them,
//!    and decrements their successors' counters; a counter reaching zero pushes that successor.
//! 4. When no node is left to claim, the callback thread spins until the nodes still running on
//!    workers complete, then copies the master buffers to the device.
//!
//! The callback thread never waits for a worker to *wake*: if no worker shows up it runs every
//! node itself, so the worst case is the single-threaded cost plus one wake call per sleeping
//! worker. It does wait in two places: for nodes a worker has already claimed, and, while
//! claiming, for a thread that pushed a ready node between its `head.fetch_add` and the store
//! into the reserved slot (a few instructions, unless that thread is preempted there).
//!
//! # Panics and promotion
//!
//! A node that panics would leave its successors waiting forever, so every DSP thread runs
//! under [`AbortOnPanic`]: a panic while processing aborts the process instead of hanging the
//! audio callback. When the core asks for real-time workers, a worker whose promotion fails
//! exits instead of joining real-time cycles ([`Telemetry::unpromoted_workers`](crate::Telemetry::unpromoted_workers)).
//!
//! # Wake and park
//!
//! After finishing a cycle's work a worker spins for [`pool::SPIN`] watching the wake generation,
//! then announces itself asleep and parks (`std::thread::park`, a futex / `WaitOnAddress` /
//! `__ulock` wait; no lock, no allocation). The callback thread bumps the generation with one
//! atomic add and calls `Thread::unpark` (a non-blocking wake syscall) only for workers that
//! announced sleep. Both sides use sequentially consistent operations on the generation and the
//! sleep flag, so a wake is never lost. Worst case on the callback thread per cycle: one atomic
//! add, one load per worker and one non-blocking wake syscall per sleeping worker.
//!
//! # Plan lifetime
//!
//! Workers find the current plan through an atomic pointer and protect it with a per-worker
//! hazard pointer (publish, then re-check the pointer). The control thread frees a retired plan
//! only once no hazard names it ([`pool::Shared::in_use`]), so the audio thread never waits on
//! plan memory and never frees it.
//!
//! # Determinism
//!
//! A node's inputs are complete before it runs and its output buffers are not shared with any
//! node that could run concurrently (liveness-aware assignment in `Graph::compile`), sums add
//! their inputs in a fixed order, and every DSP thread runs with FTZ/DAZ set
//! ([`denormal::DenormalGuard`]). Output is therefore bit-identical for any worker count.

pub(crate) mod dag;
pub(crate) mod denormal;
pub(crate) mod pool;

/// Aborts the process if dropped during a panic, so a panicking node never stalls a cycle (its
/// successors would wait for it forever). Hold one for the duration of graph processing.
pub(crate) struct AbortOnPanic;

impl Drop for AbortOnPanic {
    fn drop(&mut self) {
        if std::thread::panicking() {
            std::process::abort();
        }
    }
}
