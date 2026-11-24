//! Header checks for WAV files in a bundle, and atomic file writes. Bundle files are not
//! trusted: a header may claim more channels or frames than the file holds.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::import::{ImportError, MAX_CHANNELS};

/// What a WAV header says, with the frame count clamped to what the file can hold.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WavInfo {
    pub spec: hound::WavSpec,
    /// Frames the header claims.
    pub claimed_frames: u64,
    /// Frames actually present: the claim clamped to the bytes after the data offset.
    pub frames: u64,
}

impl WavInfo {
    /// True when every claimed frame is present.
    pub fn complete(&self) -> bool {
        self.frames == self.claimed_frames
    }
}

/// Reads the header of `path`. Refuses zero or more than [`MAX_CHANNELS`] channels, and (with
/// `float32_only`) anything but 32-bit float samples. No allocation depends on the header.
pub(crate) fn wav_info(path: &Path, float32_only: bool) -> Result<WavInfo, ImportError> {
    let bad = |m: &str| ImportError::Decode(m.to_string());
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let reader = hound::WavReader::new(BufReader::new(file))
        .map_err(|e| ImportError::Decode(e.to_string()))?;
    let spec = reader.spec();
    let claimed = u64::from(reader.duration());
    // The reader stops at the first data byte.
    let offset = reader.into_inner().stream_position()?;
    if spec.channels == 0 || usize::from(spec.channels) > MAX_CHANNELS {
        return Err(ImportError::TooLarge("channel count"));
    }
    if float32_only
        && (spec.sample_format != hound::SampleFormat::Float || spec.bits_per_sample != 32)
    {
        return Err(bad("media is not 32-bit float WAV"));
    }
    if spec.bits_per_sample == 0 || spec.bits_per_sample > 32 || spec.sample_rate == 0 {
        return Err(bad("unsupported WAV format"));
    }
    let frame_bytes = u64::from(spec.bits_per_sample.div_ceil(8)) * u64::from(spec.channels);
    let present = len.saturating_sub(offset) / frame_bytes;
    Ok(WavInfo {
        spec,
        claimed_frames: claimed,
        frames: claimed.min(present),
    })
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp file name beside `target`, unique per process and call.
fn temp_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    target.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()))
}

/// Writes `target` atomically: `fill` writes a uniquely named temp file beside it, which is
/// synced and renamed into place; the temp file is removed on any error.
pub(crate) fn write_atomic(
    target: &Path,
    fill: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = temp_path(target);
    let result = (|| {
        fill(&temp)?;
        OpenOptions::new().write(true).open(&temp)?.sync_all()?;
        fs::rename(&temp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
