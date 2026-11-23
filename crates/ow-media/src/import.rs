//! Import: decode with symphonia, resample to the project rate with rubato, copy into the
//! bundle's `audio/` folder under a content-hash name as a 32-bit float WAV.
//!
//! Imports are bounded ([`ImportLimits`]) and never panic on corrupt input: decode errors in
//! single packets are skipped (up to [`MAX_BAD_PACKETS`]), anything else is an
//! [`ImportError`], and a panic inside a codec is caught and reported as one.

use std::fs::{self, File};
use std::io::{self, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Async, FixedAsync, Resampler, SincInterpolationParameters, WindowFunction};
use sha2::{Digest, Sha256};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::Pcm;

/// Packets that fail to decode are skipped; a file with more bad packets than this is refused.
pub const MAX_BAD_PACKETS: usize = 64;

/// Bounds on what one import may read and hold.
#[derive(Debug, Clone, Copy)]
pub struct ImportLimits {
    /// Largest source file in bytes.
    pub max_file_bytes: u64,
    /// Most decoded samples (frames times channels) held in memory; 2^27 is 512 MiB of `f32`.
    pub max_samples: u64,
    /// Most channels.
    pub max_channels: usize,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 2 << 30,
            max_samples: 1 << 27,
            max_channels: 8,
        }
    }
}

/// Why an import failed.
#[derive(Debug)]
pub enum ImportError {
    /// Reading or writing a file failed.
    Io(io::Error),
    /// The file is not decodable audio (message from the decoder).
    Decode(String),
    /// A bound in [`ImportLimits`] was exceeded.
    TooLarge(&'static str),
    /// The project rate or the source rate is unusable.
    BadRate,
    /// Resampling failed.
    Resample(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::Decode(m) => write!(f, "cannot decode: {m}"),
            Self::TooLarge(m) => write!(f, "file too large: {m}"),
            Self::BadRate => write!(f, "unusable sample rate"),
            Self::Resample(m) => write!(f, "resampling failed: {m}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<io::Error> for ImportError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Decodes `path` fully into planar `f32` at the file's own rate.
pub fn decode_file(path: &Path, limits: &ImportLimits) -> Result<Pcm, ImportError> {
    let file = File::open(path)?;
    if file.metadata()?.len() > limits.max_file_bytes {
        return Err(ImportError::TooLarge("file size"));
    }
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let limits = *limits;
    catch_unwind(AssertUnwindSafe(|| decode_inner(file, hint, &limits)))
        .unwrap_or_else(|_| Err(ImportError::Decode("decoder panicked".into())))
}

fn decode_inner(file: File, hint: Hint, limits: &ImportLimits) -> Result<Pcm, ImportError> {
    let dec = |e: SymError| ImportError::Decode(e.to_string());
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(dec)?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| ImportError::Decode("no audio track".into()))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| ImportError::Decode("no audio parameters".into()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(dec)?;

    let mut rate = 0;
    let mut planar: Vec<Vec<f32>> = Vec::new();
    let mut scratch: Vec<Vec<f32>> = Vec::new();
    let mut bad = 0;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // A truncated file still yields what was decoded so far.
            Err(SymError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(SymError::DecodeError(_)) => {
                bad += 1;
                if bad > MAX_BAD_PACKETS {
                    return Err(ImportError::Decode("too many corrupt packets".into()));
                }
                continue;
            }
            Err(e) => return Err(dec(e)),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(SymError::DecodeError(_)) => {
                bad += 1;
                if bad > MAX_BAD_PACKETS {
                    return Err(ImportError::Decode("too many corrupt packets".into()));
                }
                continue;
            }
            Err(e) => return Err(dec(e)),
        };
        let spec = buf.spec();
        let channels = spec.channels().count();
        if channels == 0 || channels > limits.max_channels {
            return Err(ImportError::TooLarge("channel count"));
        }
        if planar.is_empty() {
            rate = spec.rate();
            planar = vec![Vec::new(); channels];
        } else if rate != spec.rate() || planar.len() != channels {
            return Err(ImportError::Decode("stream format changes midway".into()));
        }
        let frames = buf.frames();
        if (planar[0].len() + frames) as u64 * channels as u64 > limits.max_samples {
            return Err(ImportError::TooLarge("duration"));
        }
        buf.copy_to_vecs_planar::<f32>(&mut scratch);
        for (dst, src) in planar.iter_mut().zip(&scratch) {
            dst.extend(src.iter().map(|s| if s.is_finite() { *s } else { 0.0 }));
        }
    }
    if planar.is_empty() || planar[0].is_empty() || rate == 0 {
        return Err(ImportError::Decode("no audio decoded".into()));
    }
    Ok(Pcm {
        sample_rate: rate,
        channels: planar,
    })
}

/// Resamples to `rate`. Output length is `ceil(frames * rate / source rate)`; the filter's
/// startup delay is removed.
pub fn resample(pcm: &Pcm, rate: u32) -> Result<Pcm, ImportError> {
    if !(1000..=768_000).contains(&rate) || pcm.sample_rate == 0 {
        return Err(ImportError::BadRate);
    }
    if rate == pcm.sample_rate {
        return Ok(pcm.clone());
    }
    let err = |e: &dyn std::fmt::Display| ImportError::Resample(e.to_string());
    let channels = pcm.channels.len();
    let frames = pcm.frames();
    let ratio = f64::from(rate) / f64::from(pcm.sample_rate);
    let params = SincInterpolationParameters::new(128, WindowFunction::BlackmanHarris2);
    let mut resampler =
        Async::<f32>::new_sinc(ratio, 1.0, &params, 1024, channels, FixedAsync::Input)
            .map_err(|e| err(&e))?;
    let needed = resampler.process_all_needed_output_len(frames);
    let mut out = vec![vec![0.0_f32; needed]; channels];
    let input = SequentialSliceOfVecs::new(&pcm.channels, channels, frames).map_err(|e| err(&e))?;
    let mut output =
        SequentialSliceOfVecs::new_mut(&mut out, channels, needed).map_err(|e| err(&e))?;
    let (_, produced) = resampler
        .process_all_into_buffer(&input, &mut output, frames, None)
        .map_err(|e| err(&e))?;
    for channel in &mut out {
        channel.truncate(produced);
    }
    Ok(Pcm {
        sample_rate: rate,
        channels: out,
    })
}

/// The result of [`import_file`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// File stem of the source, for display.
    pub name: String,
    /// Content hash (hex) the bundle file name derives from.
    pub hash: String,
    /// Path relative to the bundle, `audio/<hash>.wav`.
    pub path: String,
    /// Sample rate of the stored file (the project rate).
    pub sample_rate: u32,
    /// Channels of the stored file.
    pub channels: u16,
    /// Frames of the stored file.
    pub frames: u64,
    /// True when the bundle already held this content and nothing was written.
    pub deduplicated: bool,
}

fn content_hash(path: &Path, rate: u32) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut file = File::open(path)?;
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    hasher.update(rate.to_le_bytes());
    let digest = hasher.finalize();
    Ok(digest[..16].iter().map(|b| format!("{b:02x}")).collect())
}

/// The WAV spec media files use: 32-bit float.
pub(crate) fn wav_spec(channels: usize, sample_rate: u32) -> hound::WavSpec {
    hound::WavSpec {
        channels: channels as u16,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    }
}

/// Writes planar audio as a 32-bit float WAV.
pub fn write_wav(path: &Path, pcm: &Pcm) -> Result<(), ImportError> {
    let wav_err = |e: hound::Error| ImportError::Io(io::Error::other(e));
    let spec = wav_spec(pcm.channels.len(), pcm.sample_rate);
    let mut writer =
        hound::WavWriter::new(io::BufWriter::new(File::create(path)?), spec).map_err(wav_err)?;
    for frame in 0..pcm.frames() {
        for channel in &pcm.channels {
            writer.write_sample(channel[frame]).map_err(wav_err)?;
        }
    }
    writer.finalize().map_err(wav_err)
}

/// Imports `src` into `bundle/audio/` at `project_rate`. A repeat import of the same file at
/// the same rate finds the existing copy and writes nothing.
pub fn import_file(
    src: &Path,
    bundle: &Path,
    project_rate: u32,
    limits: &ImportLimits,
) -> Result<Imported, ImportError> {
    if !(1000..=768_000).contains(&project_rate) {
        return Err(ImportError::BadRate);
    }
    if fs::metadata(src)?.len() > limits.max_file_bytes {
        return Err(ImportError::TooLarge("file size"));
    }
    let hash = content_hash(src, project_rate)?;
    let name = src
        .file_stem()
        .map_or_else(|| "audio".into(), |s| s.to_string_lossy().into_owned());
    let target: PathBuf = bundle.join("audio").join(format!("{hash}.wav"));
    let done = |channels: u16, frames: u64, deduplicated| Imported {
        name: name.clone(),
        hash: hash.clone(),
        path: format!("audio/{hash}.wav"),
        sample_rate: project_rate,
        channels,
        frames,
        deduplicated,
    };
    if let Ok(reader) = hound::WavReader::open(&target) {
        return Ok(done(
            reader.spec().channels,
            u64::from(reader.duration()),
            true,
        ));
    }
    let pcm = resample(&decode_file(src, limits)?, project_rate)?;
    fs::create_dir_all(bundle.join("audio"))?;
    let temp = bundle.join("audio").join(format!(".{hash}.tmp"));
    write_wav(&temp, &pcm)?;
    fs::rename(&temp, &target)?;
    Ok(done(pcm.channels.len() as u16, pcm.frames() as u64, false))
}
