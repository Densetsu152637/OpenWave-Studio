//! The queue of parameter values on their way to a plugin's audio side.
//!
//! A lock-free ring carries them to the audio thread. When the ring is full the value is not
//! lost: it waits in an overflow list (the latest value of a parameter replaces an earlier one
//! that is still waiting) and is pushed as soon as the audio thread has made room. Control-side
//! only; the audio thread only ever pops.

/// A parameter value for the audio side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Edit {
    pub id: u32,
    /// What the format's processor takes (plain for CLAP, normalised for VST3).
    pub value: f64,
}

/// The control end of an edit ring, with overflow.
pub(crate) struct EditSender {
    producer: rtrb::Producer<Edit>,
    overflow: Vec<Edit>,
}

impl EditSender {
    pub(crate) fn new(producer: rtrb::Producer<Edit>) -> Self {
        Self {
            producer,
            overflow: Vec::new(),
        }
    }

    /// Queues `edit`; never drops it.
    pub(crate) fn send(&mut self, edit: Edit) {
        self.flush();
        if self.overflow.is_empty() && self.producer.push(edit).is_ok() {
            return;
        }
        match self
            .overflow
            .iter_mut()
            .find(|waiting| waiting.id == edit.id)
        {
            Some(waiting) => waiting.value = edit.value,
            None => self.overflow.push(edit),
        }
    }

    /// Moves waiting values into the ring as far as it has room. Called by `service`, so a
    /// value that waited for room arrives even when nothing else is edited.
    pub(crate) fn flush(&mut self) {
        let mut sent = 0;
        while let Some(edit) = self.overflow.get(sent) {
            if self.producer.push(*edit).is_err() {
                break;
            }
            sent += 1;
        }
        self.overflow.drain(..sent);
    }

    /// How many values wait for room.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.overflow.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_dropped_when_the_ring_is_full() {
        let (producer, mut consumer) = rtrb::RingBuffer::new(4);
        let mut sender = EditSender::new(producer);
        for id in 0..10 {
            sender.send(Edit {
                id,
                value: f64::from(id),
            });
        }
        // A newer value of a waiting parameter replaces the older one.
        sender.send(Edit { id: 9, value: 99.0 });
        assert_eq!(sender.waiting(), 6);
        let mut seen = Vec::new();
        while seen.len() < 10 {
            while let Ok(edit) = consumer.pop() {
                seen.push(edit);
            }
            sender.flush();
        }
        let ids: Vec<u32> = seen.iter().map(|e| e.id).collect();
        assert_eq!(ids, (0..10).collect::<Vec<_>>());
        assert_eq!(seen[9].value, 99.0);
        assert_eq!(sender.waiting(), 0);
    }
}
