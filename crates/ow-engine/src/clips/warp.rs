//! Warped audio clips: a clip whose [`Warp`] follows the project tempo is rendered once, offline,
//! into a buffer that already has the project's timing, and then plays like any other
//! in-memory clip. The realtime path never stretches (stage 22b).
//!
//! * [`plan`] turns a clip, its warp and the project's tempo map into [`Segment`]s: stretches
//!   of the source that must fill exact spans of the timeline. A clip with a constant tempo and
//!   no markers is one segment; a marker or a tempo change inside the clip splits it (and a
//!   tempo ramp is cut into beat-long pieces), so the stretch follows the tempo map.
//! * [`render`] stretches every segment with [`ow_dsp::stretch::stretch_to_length`] to the
//!   exact number of timeline frames it fills. The result has exactly as many frames as the
//!   clip has on the timeline, whatever the tempo map does (the lengths come from the same
//!   beat-to-sample rule the clip placement uses).
//! * [`SourceCache::warped`](super::SourceCache::warped) keeps the result in memory and under
//!   the bundle's `cache/` folder, keyed by [`key`]: the media hash, the engine rate, the
//!   pitch shift and the segment table, so it is reused until the media, the warp or the tempo
//!   map under the clip changes, and every cache file is safe to delete.
//!
//! Playing while a tempo change is still being rendered: compiling is the only place that
//! renders, and it runs on the control thread while the audio thread keeps playing the previous
//! plan, so the clip keeps its old timing until the new plan (with the finished render) is
//! swapped in. Nothing plays unstretched or silent in between.

use ow_dsp::stretch::{MAX_RATIO, MIN_RATIO, stretch_to_length};
use ow_media::Pcm;
use ow_model::{Warp, WarpMarker, WarpMode};

use crate::Timeline;

/// Version of the rendering (bump when the output for the same inputs can change, so older
/// cache files are not matched).
pub const RENDER_VERSION: u32 = 1;

/// The most frames a warped render may hold per channel (about 45 minutes at 48 kHz); a longer
/// clip plays unstretched.
pub const MAX_WARPED_FRAMES: u64 = 1 << 27;

/// The most segments one clip is cut into; a longer clip gets coarser ramp pieces.
const MAX_SEGMENTS: usize = 2048;

/// One stretch of the source that fills `frames` timeline frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// First source frame (at the engine rate; may be before the file for a clip that starts
    /// ahead of it, which reads as silence).
    pub source_start: i64,
    /// One past the last source frame.
    pub source_end: i64,
    /// Timeline frames this segment fills.
    pub frames: u64,
}

/// Whether `warp` asks for stretching.
pub fn active(warp: &Warp) -> bool {
    warp.mode == WarpMode::FollowTempo
}

/// The source position, in engine-rate frames, of clip-local beat `beat`.
fn source_at(beat: f64, warp: &Warp, offset: f64, scale: f64, rate: f64) -> f64 {
    let per_beat = 60.0 / warp.source_bpm * rate;
    let markers = &warp.markers;
    let (Some(first), Some(last)) = (markers.first(), markers.last()) else {
        return offset + beat * per_beat;
    };
    let frame = |m: &WarpMarker| m.source_frame as f64 * scale;
    if beat <= first.beat {
        return frame(first) + (beat - first.beat) * per_beat;
    }
    if beat >= last.beat {
        return frame(last) + (beat - last.beat) * per_beat;
    }
    // The first marker at or after `beat`; the one before it exists because `beat > first`.
    let next = markers.partition_point(|m| m.beat < beat);
    let (a, b) = (&markers[next - 1], &markers[next]);
    let t = (beat - a.beat) / (b.beat - a.beat);
    frame(a) + (frame(b) - frame(a)) * t
}

/// The segments that fill a clip of `length` beats starting at beat `start`.
///
/// `source_offset` counts frames of the file (at `file_rate`) and is the source position of the
/// clip's first beat when the warp has no markers; with markers, their source frames place the
/// source and the offset is not used. `rate` is the engine rate the source was converted to.
pub fn plan(
    start: f64,
    length: f64,
    warp: &Warp,
    source_offset: u64,
    file_rate: u32,
    timeline: &Timeline,
    rate: u32,
) -> Vec<Segment> {
    let scale = f64::from(rate) / f64::from(file_rate.max(1));
    let offset = source_offset as f64 * scale;
    let mut beats = vec![0.0, length];
    beats.extend(
        warp.markers
            .iter()
            .map(|m| m.beat)
            .filter(|b| *b > 0.0 && *b < length),
    );
    let points = timeline.tempo().points();
    beats.extend(
        points
            .iter()
            .map(|p| p.beat - start)
            .filter(|b| *b > 0.0 && *b < length),
    );
    // A ramp changes the speed continuously: cut the clip into pieces of at most a beat (fewer
    // when the clip is very long) wherever the tempo map ramps.
    let ramps = points
        .windows(2)
        .any(|w| w[0].ramp && w[0].beat < start + length && w[1].beat > start);
    if ramps {
        let step = (length / (MAX_SEGMENTS / 2) as f64).max(1.0);
        let mut b = step;
        while b < length {
            beats.push(b);
            b += step;
        }
    }
    beats.sort_by(f64::total_cmp);
    beats.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let sample = |beat: f64| timeline.beat_to_sample_index(start + beat, rate);
    beats
        .windows(2)
        .filter_map(|pair| {
            let frames = sample(pair[1]).saturating_sub(sample(pair[0]));
            let from = source_at(pair[0], warp, offset, scale, f64::from(rate)).round();
            let to = source_at(pair[1], warp, offset, scale, f64::from(rate)).round();
            (frames > 0).then_some(Segment {
                source_start: from as i64,
                source_end: (to as i64).max(from as i64),
                frames,
            })
        })
        .collect()
}

/// Total timeline frames of `segments`.
pub fn total_frames(segments: &[Segment]) -> u64 {
    segments.iter().map(|s| s.frames).sum()
}

/// FNV-1a over `bytes`, continuing from `hash`.
fn fnv(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// The cache key of a render: stable across runs and builds (it names a file on disk).
pub fn key(media_hash: &str, rate: u32, channels: usize, pitch: f32, segments: &[Segment]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    hash = fnv(hash, &RENDER_VERSION.to_le_bytes());
    hash = fnv(hash, media_hash.as_bytes());
    hash = fnv(hash, &rate.to_le_bytes());
    hash = fnv(hash, &(channels as u64).to_le_bytes());
    hash = fnv(hash, &pitch.to_bits().to_le_bytes());
    for s in segments {
        hash = fnv(hash, &s.source_start.to_le_bytes());
        hash = fnv(hash, &s.source_end.to_le_bytes());
        hash = fnv(hash, &s.frames.to_le_bytes());
    }
    hash
}

/// The pitch shift of a warp in semitones as the stretcher takes it.
pub fn pitch(warp: &Warp) -> f32 {
    warp.transpose_semitones as f32
}

/// Renders `segments` of `source` (at the engine rate) into one buffer of
/// [`total_frames`] frames: each segment is stretched to the frames it fills, a segment whose
/// tempo change is beyond the stretcher's range (4x either way) is silent, and a segment that
/// needs no stretching and no pitch shift is copied.
pub fn render(source: &Pcm, segments: &[Segment], pitch_semitones: f32) -> Pcm {
    let channels = source.channels.len();
    let total = total_frames(segments) as usize;
    let mut out = vec![Vec::with_capacity(total); channels];
    let len = source.frames() as i64;
    for segment in segments {
        let frames = segment.frames as usize;
        let want = (segment.source_end - segment.source_start).max(0) as usize;
        let slice = |channel: &Vec<f32>| -> Vec<f32> {
            (segment.source_start..segment.source_end)
                .map(|i| {
                    if (0..len).contains(&i) {
                        channel[i as usize]
                    } else {
                        0.0
                    }
                })
                .collect()
        };
        let ratio = frames as f64 / want.max(1) as f64;
        if want == 0 || !(MIN_RATIO..=MAX_RATIO).contains(&ratio) {
            for channel in &mut out {
                channel.resize(channel.len() + frames, 0.0);
            }
        } else if want == frames && pitch_semitones == 0.0 {
            for (channel, src) in out.iter_mut().zip(&source.channels) {
                channel.extend(slice(src));
            }
        } else {
            let input: Vec<Vec<f32>> = source.channels.iter().map(slice).collect();
            let views: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
            let stretched = stretch_to_length(
                &views,
                channels,
                source.sample_rate,
                frames,
                pitch_semitones,
            );
            for (channel, part) in out.iter_mut().zip(stretched) {
                channel.extend(part);
            }
        }
    }
    Pcm {
        sample_rate: source.sample_rate,
        channels: out,
    }
}
