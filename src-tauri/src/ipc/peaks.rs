//! The peak-range command's data source: the `.owpk` peak pyramids in the project's `cache/`
//! folder ([`ow_media::peaks`]), encoded in the `ow_ipc::peaks` wire format. Media without a
//! registered or readable peak file answers an empty range.

use std::path::PathBuf;

use ow_ipc::peaks::encode_peaks;
use ow_media::peaks::PeakCache;

/// Media id to peak file, served as raw response bytes.
#[derive(Default)]
pub struct PeakStore {
    cache: PeakCache,
}

impl PeakStore {
    /// Points `media` at its `.owpk` file (what the importer registers once the peaks are
    /// built).
    pub fn register(&self, media: u64, owpk: PathBuf) {
        self.cache.register(media, owpk);
    }

    /// The response bytes for `bins` min/max pairs over frames `[start, end)` of `media`.
    pub fn range(&self, media: u64, start: u64, end: u64, bins: u32) -> Vec<u8> {
        encode_peaks(&self.cache.range(media, start, end, bins as usize))
    }
}

#[cfg(test)]
mod tests {
    use ow_ipc::peaks::{decode_peaks, min_max};
    use ow_media::Pcm;
    use ow_media::peaks::Pyramid;

    use super::*;

    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| ((i * 37) % 1000) as f32 / 500.0 - 1.0)
            .collect()
    }

    #[test]
    fn the_range_is_served_from_the_peak_file_in_the_wire_format() {
        let samples = ramp(64 * 1000);
        let dir = tempfile::tempdir().unwrap();
        let owpk = dir.path().join("cache").join("m.owpk");
        Pyramid::from_pcm(&Pcm {
            sample_rate: 48_000,
            channels: vec![samples.clone()],
        })
        .write(&owpk)
        .unwrap();

        let store = PeakStore::default();
        store.register(7, owpk);
        // Bins of 64 frames are exact: the bytes equal the stub's output over the raw audio.
        let bytes = store.range(7, 0, 64 * 1000, 1000);
        let want = encode_peaks(&min_max(&samples, 0, 64 * 1000, 1000));
        assert_eq!(bytes, want);
        assert_eq!(decode_peaks(&bytes).unwrap().len(), 1000);
        // The header words are the documented ones: version 1, then the bin count.
        assert_eq!(&bytes[..8], &[1, 0, 0, 0, 0xE8, 3, 0, 0]);

        // Unknown media, a range past the end and no bins answer an empty (valid) response.
        let empty = encode_peaks(&[]);
        assert_eq!(store.range(8, 0, 1000, 4), empty);
        assert_eq!(store.range(7, 10_000_000, 20_000_000, 4), empty);
        assert_eq!(store.range(7, 0, 1000, 0), empty);
    }
}
