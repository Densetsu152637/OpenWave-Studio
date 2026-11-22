//! The peak-range command's data source. Stage 11 replaces this with the real peak cache
//! (`cache/*.owpk` pyramids); until then it serves min/max computed from mono buffers kept in
//! memory, and answers an empty range for media it does not hold. The wire format is
//! `ow_ipc::peaks` and stays.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use ow_ipc::peaks::{encode_peaks, min_max};

/// In-memory mono sample buffers by media id.
#[derive(Default)]
pub struct PeakStore {
    buffers: Mutex<HashMap<u64, Arc<Vec<f32>>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl PeakStore {
    /// Holds `samples` (mono) for `media`, replacing an earlier buffer.
    pub fn register(&self, media: u64, samples: Vec<f32>) {
        lock(&self.buffers).insert(media, Arc::new(samples));
    }

    /// The response bytes for `bins` min/max pairs over frames `[start, end)` of `media`.
    pub fn range(&self, media: u64, start: u64, end: u64, bins: u32) -> Vec<u8> {
        let buffer = lock(&self.buffers).get(&media).cloned();
        let pairs = match buffer {
            Some(samples) => {
                let clamp = |frame: u64| usize::try_from(frame).unwrap_or(usize::MAX);
                min_max(&samples, clamp(start), clamp(end), bins as usize)
            }
            None => Vec::new(),
        };
        encode_peaks(&pairs)
    }
}

#[cfg(test)]
mod tests {
    use ow_ipc::peaks::decode_peaks;

    use super::*;

    #[test]
    fn registered_media_serves_min_max_pairs_and_unknown_media_is_empty() {
        let store = PeakStore::default();
        store.register(
            7,
            (0..1000).map(|i| (i as f32 / 1000.0) * 2.0 - 1.0).collect(),
        );
        let pairs = decode_peaks(&store.range(7, 0, 1000, 4)).unwrap();
        assert_eq!(pairs.len(), 4);
        assert_eq!(pairs[0].0, -1.0);
        assert!(pairs[3].1 > 0.99);
        assert!(
            pairs.windows(2).all(|w| w[0].1 <= w[1].0 + 0.01),
            "ranges ascend"
        );
        assert!(
            decode_peaks(&store.range(8, 0, 1000, 4))
                .unwrap()
                .is_empty()
        );
        assert!(
            decode_peaks(&store.range(7, 5000, 6000, 4))
                .unwrap()
                .is_empty()
        );
    }
}
