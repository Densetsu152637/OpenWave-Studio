//! Wait-free single-producer rings that feed the audio thread.
//!
//! `rtrb` rings are single-producer, so every producer that talks to the audio thread owns its
//! own ring (control thread, each MIDI input callback thread, control-surface input). The
//! producer holds a [`RingSender`]; the audio thread holds the matching [`ProducerRing`] and
//! drains all of them at the start of each cycle. Capacity is fixed at creation and nothing here
//! allocates after that.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Creates a ring of `capacity` items (at least 1) and returns its two ends.
///
/// Allocates once, on the calling (non-real-time) thread.
pub fn producer_ring<T>(capacity: usize) -> (RingSender<T>, ProducerRing<T>) {
    let (producer, consumer) = rtrb::RingBuffer::new(capacity.max(1));
    let overflow = Arc::new(AtomicU64::new(0));
    (
        RingSender {
            producer,
            overflow: Arc::clone(&overflow),
        },
        ProducerRing { consumer, overflow },
    )
}

/// Producer end of a ring. Owned by exactly one thread; `push` never blocks or allocates.
pub struct RingSender<T> {
    producer: rtrb::Producer<T>,
    overflow: Arc<AtomicU64>,
}

impl<T> RingSender<T> {
    /// Sends `item`. When the ring is full the newest item (this one) is dropped and the
    /// overflow counter is incremented; returns whether the item was queued.
    ///
    /// The dropped item is destroyed on the calling thread, so push only plain (`Copy`-like)
    /// messages from real-time producers such as a MIDI callback.
    pub fn push(&mut self, item: T) -> bool {
        if self.producer.push(item).is_ok() {
            true
        } else {
            self.overflow.fetch_add(1, Ordering::Relaxed);
            false
        }
    }

    /// Sends `item` or hands it back when the ring is full, without touching the overflow
    /// counter. Used for rings whose items must never be lost (plans, retired plans).
    pub fn try_push(&mut self, item: T) -> Result<(), T> {
        self.producer
            .push(item)
            .map_err(|rtrb::PushError::Full(item)| item)
    }

    /// Number of items dropped by [`push`](Self::push) because the ring was full.
    pub fn overflow_count(&self) -> u64 {
        self.overflow.load(Ordering::Relaxed)
    }

    /// Free slots right now.
    pub fn slots(&self) -> usize {
        self.producer.slots()
    }
}

/// Consumer end of one producer's ring, drained by the thread that owns the engine cycle.
///
/// Contract for later stages: a new producer (MIDI input thread, control surface, disk thread)
/// calls [`producer_ring`] (or [`CoreBuilder::producer_ring`](crate::CoreBuilder::producer_ring)),
/// keeps the [`RingSender`] and registers the `ProducerRing` with the engine before the stream
/// starts, so the audio thread never grows its ring list. Items from one ring arrive in order;
/// ordering across rings is not defined.
pub struct ProducerRing<T> {
    consumer: rtrb::Consumer<T>,
    overflow: Arc<AtomicU64>,
}

impl<T> ProducerRing<T> {
    /// Takes the oldest queued item, if any. Wait-free.
    pub fn pop(&mut self) -> Option<T> {
        self.consumer.pop().ok()
    }

    /// Items dropped so far because the ring was full (see [`RingSender::push`]).
    pub fn overflow_count(&self) -> u64 {
        self.overflow.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_drops_newest_and_counts() {
        let (mut tx, mut rx) = producer_ring::<u32>(2);
        assert!(tx.push(1));
        assert!(tx.push(2));
        assert!(!tx.push(3));
        assert_eq!(tx.overflow_count(), 1);
        assert_eq!(rx.overflow_count(), 1);
        assert_eq!((rx.pop(), rx.pop(), rx.pop()), (Some(1), Some(2), None));
    }

    #[test]
    fn try_push_returns_the_item_without_counting() {
        let (mut tx, _rx) = producer_ring::<u32>(1);
        assert!(tx.try_push(7).is_ok());
        assert_eq!(tx.try_push(8), Err(8));
        assert_eq!(tx.overflow_count(), 0);
    }
}
