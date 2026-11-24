//! Per-cycle dependency counters and the lock-free ready queue of one plan.
//!
//! Every cycle each node becomes ready exactly once, so the ready queue is a fixed array with one
//! slot per node, filled in push order and consumed in the same order:
//!
//! - `head` counts pushes (`fetch_add`), `tail` counts pops (compare-exchange), and both carry
//!   the cycle's epoch in their high 32 bits.
//! - A slot holds `epoch << 32 | node`. A popper only takes slot `i` once it carries the current
//!   epoch, so a slot left over from an earlier cycle (or not yet written in this one) reads as
//!   empty, and a stale compare-exchange from an earlier cycle fails because the epoch changed.
//! - A node is pushed by whichever thread drops its dependency counter to zero; the
//!   acquire-release chain on the counter and the slot publishes the predecessors' output
//!   buffers to the thread that pops it.
//!
//! No operation allocates, locks or blocks.

// `begin` is an `unsafe fn`: its caller guarantees that no cycle is running.
#![allow(unsafe_code)]

use std::hint::spin_loop;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

fn tag(epoch: u32, low: u32) -> u64 {
    (u64::from(epoch) << 32) | u64::from(low)
}

fn split(word: u64) -> (u32, u32) {
    ((word >> 32) as u32, word as u32)
}

/// Result of one attempt to take a ready node.
pub(crate) enum Pop {
    /// This thread now owns node `node` for the cycle with epoch `epoch`.
    Node { node: usize, epoch: u32 },
    /// No node is ready right now, but the cycle still has unclaimed nodes.
    Empty,
    /// Every node of the current cycle has been claimed (or no cycle has started).
    Done,
}

/// The dependency graph of a plan plus the per-cycle scheduling state.
pub(crate) struct Dag {
    /// Number of distinct predecessors of each node.
    deps: Box<[u32]>,
    /// Successors of node `i` are `succ[succ_start[i]..succ_start[i + 1]]`.
    succ_start: Box<[u32]>,
    succ: Box<[u32]>,
    /// Nodes without predecessors, pushed when a cycle begins.
    roots: Box<[u32]>,
    pending: Box<[AtomicU32]>,
    ready: Box<[AtomicU64]>,
    head: AtomicU64,
    tail: AtomicU64,
    remaining: AtomicU32,
    /// Epoch of the current cycle. Written only by the thread that runs `begin`.
    epoch: AtomicU32,
}

impl Dag {
    /// A DAG over `count` nodes with dependency `edges` `(from, to)`; duplicates are merged.
    /// Every edge must go from a lower to a higher node index (topological numbering).
    pub(crate) fn new(count: usize, edges: &[(usize, usize)]) -> Self {
        let mut pairs: Vec<(usize, usize)> = edges.to_vec();
        pairs.sort_unstable();
        pairs.dedup();
        debug_assert!(pairs.iter().all(|&(from, to)| from < to && to < count));
        let mut deps = vec![0_u32; count];
        let mut succ_start = vec![0_u32; count + 1];
        for &(from, to) in &pairs {
            deps[to] += 1;
            succ_start[from + 1] += 1;
        }
        // Prefix sum: successors of `i` start after those of every lower node.
        let mut total = 0;
        for start in &mut succ_start {
            total += *start;
            *start = total;
        }
        let succ = pairs.iter().map(|&(_, to)| to as u32).collect();
        let roots = (0..count)
            .filter(|&index| deps[index] == 0)
            .map(|index| index as u32)
            .collect();
        let count_word = count as u32;
        Self {
            deps: deps.into_boxed_slice(),
            succ_start: succ_start.into_boxed_slice(),
            succ,
            roots,
            pending: (0..count).map(|_| AtomicU32::new(0)).collect(),
            ready: (0..count).map(|_| AtomicU64::new(0)).collect(),
            head: AtomicU64::new(tag(0, count_word)),
            // Epoch 0 with every slot claimed: a worker that looks before the first cycle sees
            // `Done`.
            tail: AtomicU64::new(tag(0, count_word)),
            remaining: AtomicU32::new(0),
            epoch: AtomicU32::new(0),
        }
    }

    /// Number of nodes.
    pub(crate) fn len(&self) -> usize {
        self.deps.len()
    }

    /// Resets the counters and publishes the root nodes for a new cycle.
    ///
    /// # Safety
    ///
    /// Only one thread (the audio thread) calls `begin`, and only after [`wait_done`] of the
    /// previous cycle returned, so no node of this DAG is being processed.
    ///
    /// [`wait_done`]: Self::wait_done
    pub(crate) unsafe fn begin(&self) {
        let mut epoch = self.epoch.load(Ordering::Relaxed).wrapping_add(1);
        if epoch == 0 {
            // Epoch 0 is the "no cycle yet" state that zeroed slots carry.
            epoch = 1;
        }
        self.epoch.store(epoch, Ordering::Relaxed);
        for (pending, &deps) in self.pending.iter().zip(self.deps.iter()) {
            pending.store(deps, Ordering::Relaxed);
        }
        self.remaining.store(self.len() as u32, Ordering::Relaxed);
        for (slot, &root) in self.ready.iter().zip(self.roots.iter()) {
            slot.store(tag(epoch, root), Ordering::Relaxed);
        }
        self.head
            .store(tag(epoch, self.roots.len() as u32), Ordering::Relaxed);
        // Publishes everything above: a thread that reads this tail sees the new counters.
        self.tail.store(tag(epoch, 0), Ordering::Release);
    }

    /// Tries to claim the next ready node.
    pub(crate) fn pop(&self) -> Pop {
        let current = self.tail.load(Ordering::Acquire);
        let (epoch, index) = split(current);
        let Some(slot) = self.ready.get(index as usize) else {
            return Pop::Done;
        };
        let (slot_epoch, node) = split(slot.load(Ordering::Acquire));
        if slot_epoch != epoch {
            // Reserved by a pusher that has not written it yet, or stale.
            return Pop::Empty;
        }
        match self.tail.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Relaxed,
        ) {
            Ok(_) => Pop::Node {
                node: node as usize,
                epoch,
            },
            Err(_) => Pop::Empty,
        }
    }

    /// Marks `node` (claimed in cycle `epoch`) finished: pushes successors that became ready
    /// and counts the node done. Call after the node's outputs are written.
    pub(crate) fn complete(&self, node: usize, epoch: u32) {
        let start = self.succ_start[node] as usize;
        let end = self.succ_start[node + 1] as usize;
        for &next in &self.succ[start..end] {
            if self.pending[next as usize].fetch_sub(1, Ordering::AcqRel) == 1 {
                let (_, index) = split(self.head.fetch_add(1, Ordering::Relaxed));
                self.ready[index as usize].store(tag(epoch, next), Ordering::Release);
            }
        }
        self.remaining.fetch_sub(1, Ordering::AcqRel);
    }

    /// Spins until every node of the current cycle has completed. Only the thread that called
    /// [`begin`](Self::begin) waits; it returns once the nodes other threads claimed are done.
    pub(crate) fn wait_done(&self) {
        while self.remaining.load(Ordering::Acquire) != 0 {
            spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(dag: &Dag) -> Vec<usize> {
        let mut order = Vec::new();
        loop {
            match dag.pop() {
                Pop::Node { node, epoch } => {
                    order.push(node);
                    dag.complete(node, epoch);
                }
                Pop::Empty => panic!("single-threaded drain found an unwritten slot"),
                Pop::Done => return order,
            }
        }
    }

    #[test]
    fn nothing_is_ready_before_the_first_cycle() {
        let dag = Dag::new(3, &[(0, 1), (1, 2)]);
        assert!(matches!(dag.pop(), Pop::Done));
    }

    #[test]
    fn every_node_runs_once_per_cycle_after_its_predecessors() {
        // Diamond plus an independent node and a duplicate edge.
        let edges = [(0, 1), (0, 2), (1, 3), (2, 3), (2, 3)];
        let dag = Dag::new(5, &edges);
        for _ in 0..3 {
            // SAFETY: single thread, previous cycle drained.
            unsafe { dag.begin() };
            let order = drain(&dag);
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, vec![0, 1, 2, 3, 4]);
            let position = |n| order.iter().position(|&x| x == n).unwrap();
            for (from, to) in edges {
                assert!(position(from) < position(to), "{order:?}");
            }
            dag.wait_done();
        }
    }
}
