import type { TempoMap } from "../arrange/time";
import type { Clip, Warp } from "../ipc/types";

/**
 * Warp maths shared by the clip edits and the warnings. It mirrors `ow-engine::clips::warp`:
 * clip-local beats map to source frames of the media file (at the media's own sample rate)
 * through the markers, which pin a source frame to a beat, and continue at the source tempo
 * before the first and after the last marker. Without markers the clip's `source_offset` is the
 * source position of beat 0.
 */

/** The stretcher's range: the ratio of output to input duration, either way. */
export const WARP_RATIO_LIMIT = 4;

/** The most pieces a ramp is cut into (the engine uses 1024 for a long clip). */
const MAX_RAMP_PIECES = 1024;

/** The source frame at clip-local `beat`. */
export function sourceFrameAt(
  warp: Pick<Warp, "source_bpm" | "markers">,
  beat: number,
  mediaRate: number,
  sourceOffset: number,
): number {
  const perBeat = (60 / warp.source_bpm) * mediaRate;
  const markers = warp.markers;
  const first = markers[0];
  const last = markers[markers.length - 1];
  if (!first || !last) return sourceOffset + beat * perBeat;
  if (beat <= first.beat) return first.source_frame + (beat - first.beat) * perBeat;
  if (beat >= last.beat) return last.source_frame + (beat - last.beat) * perBeat;
  const next = markers.findIndex((m) => m.beat >= beat);
  const a = markers[next - 1]!;
  const b = markers[next]!;
  const t = (beat - a.beat) / (b.beat - a.beat);
  return a.source_frame + (b.source_frame - a.source_frame) * t;
}

/**
 * The warp for a clip whose start moved `delta` beats later (a left trim, or the right part of
 * a split) without moving the audio: marker beats are clip-local, so they move back by `delta`
 * and keep their source frames. Markers that fall before the new start are replaced by one at
 * beat 0 on the line they defined, so the stretch of what remains does not change.
 */
export function shiftWarpMarkers(warp: Warp, delta: number, mediaRate: number): Warp {
  if (warp.markers.length === 0 || delta === 0) return warp;
  const shifted = warp.markers.map((m) => ({ ...m, beat: m.beat - delta }));
  if (shifted.every((m) => m.beat >= 0)) return { ...warp, markers: shifted };
  const kept = shifted.filter((m) => m.beat >= 0);
  const start = Math.max(0, Math.round(sourceFrameAt(warp, delta, mediaRate, 0)));
  const next = kept[0];
  if ((kept[0]?.beat ?? 1) > 0 && (!next || start < next.source_frame)) {
    kept.unshift({ source_frame: start, beat: 0 });
  }
  return { ...warp, markers: kept };
}

/**
 * Whether a warped clip needs a stretch beyond the engine's limit somewhere (the part of the
 * clip that is silent): the ratio of project time to source time, between the clip's markers
 * and sampled along its length, is above 4 or below 1/4.
 */
export function warpOutOfRange(clip: Clip, tempo: TempoMap, mediaRate: number): boolean {
  if (clip.content.type !== "audio") return false;
  const warp = clip.content.warp;
  if (!warp || warp.mode !== "follow_tempo" || !(warp.source_bpm > 0)) return false;
  const offset = clip.content.source_offset;
  const beats = new Set<number>([0, clip.length]);
  for (const m of warp.markers) if (m.beat > 0 && m.beat < clip.length) beats.add(m.beat);
  // The engine's own cuts: every tempo change inside the clip, and beat-long pieces where the
  // tempo map ramps, so a short out-of-range stretch between two tempo changes is not averaged
  // away by a coarse sample.
  const points = tempo.points;
  for (const p of points) {
    const local = p.beat - clip.start;
    if (local > 0 && local < clip.length) beats.add(local);
  }
  const end = clip.start + clip.length;
  const ramps = points.some((p, i) => {
    const next = points[i + 1];
    return p.ramp && next !== undefined && p.beat < end && next.beat > clip.start;
  });
  if (ramps) {
    const step = Math.max(1, clip.length / MAX_RAMP_PIECES);
    for (let b = step; b < clip.length; b += step) beats.add(b);
  }
  const sorted = [...beats].sort((a, b) => a - b);
  for (let i = 0; i + 1 < sorted.length; i++) {
    const [from, to] = [sorted[i]!, sorted[i + 1]!];
    const seconds =
      tempo.beatsToSeconds(clip.start + to) - tempo.beatsToSeconds(clip.start + from);
    const source =
      (sourceFrameAt(warp, to, mediaRate, offset) - sourceFrameAt(warp, from, mediaRate, offset)) /
      mediaRate;
    if (!(source > 0) || !(seconds > 0)) continue;
    const ratio = seconds / source;
    if (ratio > WARP_RATIO_LIMIT + 1e-9 || ratio < 1 / WARP_RATIO_LIMIT - 1e-9) return true;
  }
  return false;
}
