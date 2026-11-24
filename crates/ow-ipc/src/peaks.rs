//! Waveform peak ranges as raw bytes (the wire contract; `ow-media` supplies the pairs).
//!
//! The UI asks for `bins` min/max pairs covering the frame range `[start, end)` of one media
//! item through the `peaks_range` command, which answers with a `tauri::ipc::Response` holding the
//! bytes [`encode_peaks`] builds.
//!
//! # Response layout (little-endian)
//!
//! | offset | content |
//! | --- | --- |
//! | 0 | `u32` format version, `1` |
//! | 4 | `u32` bin count `n` |
//! | `8 + 8 i` | `f32` minimum of bin `i` |
//! | `12 + 8 i` | `f32` maximum of bin `i` |
//!
//! Bins split the range evenly (the last may be shorter). A range with no audio (unknown media,
//! empty range, `bins == 0`) answers `n = 0`.
//!
//! # Source
//!
//! The app serves pairs from the `.owpk` peak pyramids of `ow-media` (min/max per 64, 512 and
//! 4096 frames). [`min_max`] computes the same pairs directly from a mono buffer and is the
//! reference the tests compare the cache against.

/// Wire format version in the first word of a peak response.
pub const PEAKS_VERSION: u32 = 1;
/// Most bins one request may ask for.
pub const MAX_BINS: usize = 16_384;

/// Min/max pairs of `samples[start..end]` (clamped to the buffer) in `bins` bins.
pub fn min_max(samples: &[f32], start: usize, end: usize, bins: usize) -> Vec<(f32, f32)> {
    let end = end.min(samples.len());
    let bins = bins.min(MAX_BINS);
    if bins == 0 || start >= end {
        return Vec::new();
    }
    let len = end - start;
    let bins = bins.min(len);
    (0..bins)
        .map(|bin| {
            let from = start + bin * len / bins;
            let to = start + (bin + 1) * len / bins;
            samples[from..to]
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| {
                    (lo.min(*s), hi.max(*s))
                })
        })
        .collect()
}

/// Packs pairs into the response layout.
pub fn encode_peaks(pairs: &[(f32, f32)]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + 8 * pairs.len());
    bytes.extend_from_slice(&PEAKS_VERSION.to_le_bytes());
    bytes.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
    for (lo, hi) in pairs {
        bytes.extend_from_slice(&lo.to_le_bytes());
        bytes.extend_from_slice(&hi.to_le_bytes());
    }
    bytes
}

/// Reads a response back into pairs (`None` when malformed).
pub fn decode_peaks(bytes: &[u8]) -> Option<Vec<(f32, f32)>> {
    let word = |at: usize| bytes.get(at..at + 4).map(|b| [b[0], b[1], b[2], b[3]]);
    if u32::from_le_bytes(word(0)?) != PEAKS_VERSION {
        return None;
    }
    let count = u32::from_le_bytes(word(4)?) as usize;
    if bytes.len() != 8 + 8 * count {
        return None;
    }
    Some(
        (0..count)
            .map(|i| {
                let at = 8 + 8 * i;
                (
                    f32::from_le_bytes(word(at).unwrap_or_default()),
                    f32::from_le_bytes(word(at + 4).unwrap_or_default()),
                )
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bins_cover_the_range_and_round_trip() {
        let samples: Vec<f32> = (0..100).map(|i| (i as f32) / 100.0 - 0.5).collect();
        let pairs = min_max(&samples, 0, 100, 10);
        assert_eq!(pairs.len(), 10);
        assert_eq!(pairs[0].0, -0.5);
        assert!((pairs[9].1 - 0.49).abs() < 1e-6);
        assert_eq!(decode_peaks(&encode_peaks(&pairs)).unwrap(), pairs);
    }

    #[test]
    fn empty_and_out_of_range_requests_answer_no_bins() {
        let samples = [0.1_f32, -0.2, 0.3];
        assert!(min_max(&samples, 2, 2, 4).is_empty());
        assert!(min_max(&samples, 9, 20, 4).is_empty());
        assert!(min_max(&samples, 0, 3, 0).is_empty());
        assert_eq!(decode_peaks(&encode_peaks(&[])).unwrap(), Vec::new());
        // More bins than samples collapses to one bin per sample.
        assert_eq!(min_max(&samples, 0, 3, 50).len(), 3);
    }
}
