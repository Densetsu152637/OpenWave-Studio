import type { Clip, MediaItem } from "../ipc/types";
import type { TempoMap } from "./time";

/** Frames a peak bin must span at least (the peak pyramid's finest level). */
export const MIN_BIN_FRAMES = 64;

export interface PeakRequest {
  media: number;
  /** First media frame, at the media's own rate. */
  start: number;
  /** One past the last media frame. */
  end: number;
  bins: number;
}

/**
 * The peak range to ask for to draw columns `[left, right)` (pixels relative to the clip's left
 * edge) of an audio clip `width` pixels wide: one bin per pixel, but never narrower than
 * `MIN_BIN_FRAMES` frames. The clip's frames run linearly across its width (exact at constant
 * tempo). Null when nothing of the clip's media falls in the columns.
 */
export function peakRequest(
  clip: Clip,
  media: Pick<MediaItem, "sample_rate" | "frames">,
  tempo: TempoMap,
  width: number,
  left: number,
  right: number,
): PeakRequest | null {
  if (clip.content.type !== "audio" || width <= 0 || right <= left) return null;
  const seconds = tempo.beatsToSeconds(clip.start + clip.length) - tempo.beatsToSeconds(clip.start);
  const first = clip.content.source_offset;
  const last = Math.min(media.frames, first + seconds * media.sample_rate);
  if (!(last > first)) return null;
  const start = Math.floor(first + ((last - first) * left) / width);
  const end = Math.min(media.frames, Math.ceil(first + ((last - first) * right) / width));
  if (end <= start) return null;
  const bins = Math.max(1, Math.min(Math.ceil(right - left), Math.floor((end - start) / MIN_BIN_FRAMES)));
  return { media: clip.content.media, start, end, bins };
}

/**
 * Fits `bins` min/max pairs (interleaved) to `columns` pixel columns: a column takes the extremes
 * of the bins it covers, or the bin under it when there are fewer bins than columns.
 */
export function waveformColumns(data: Float32Array, columns: number): Float32Array {
  const bins = Math.floor(data.length / 2);
  const out = new Float32Array(columns * 2);
  if (bins === 0) return out;
  for (let c = 0; c < columns; c++) {
    const from = Math.floor((c * bins) / columns);
    const to = Math.max(from + 1, Math.floor(((c + 1) * bins) / columns));
    let min = Infinity;
    let max = -Infinity;
    for (let b = from; b < Math.min(to, bins); b++) {
      min = Math.min(min, data[2 * b]!);
      max = Math.max(max, data[2 * b + 1]!);
    }
    out[2 * c] = min;
    out[2 * c + 1] = max;
  }
  return out;
}
