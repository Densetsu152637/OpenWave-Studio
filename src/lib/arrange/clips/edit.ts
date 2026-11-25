import type { Clip, EditCommand, MediaItem } from "../../ipc/types";
import { inTransaction, type ArrangeApi } from "../edits";
import { snapBeat, type SnapResolution } from "../snap";
import type { SignatureMap, TempoMap } from "../time";
import type { View } from "../view";

/**
 * Clip editing in the arrangement: trim, fades, split, duplicate, gain and mute. Everything here
 * is pure (a clip in, a clip out) or one transaction of `ReplaceClip`/`AddClip` commands, so every
 * edit is one undo step and nothing needs a command the model does not already have.
 */

/** The shortest a clip can be trimmed or split to, in beats. */
export const MIN_CLIP_BEATS = 1 / 64;
/** Pixels at a clip's left and right edge that start a trim. */
export const EDGE_PX = 6;
/** Clip gain limits in dB (the floor stands in for minus infinity). */
export const MIN_GAIN_DB = -60;
export const MAX_GAIN_DB = 12;

/** What the media of an audio clip must say for edits that touch its source. */
export type MediaBounds = Pick<MediaItem, "sample_rate" | "frames">;

/** What a drag on a clip's edge or fade handle changes. */
export type AdjustMode = "trim-left" | "trim-right" | "fade-in" | "fade-out";

/** What a drag on a clip does: move it, or one of the handle drags. */
export type DragMode = "move" | AdjustMode;

const clamp = (value: number, low: number, high: number) => Math.min(high, Math.max(low, value));

/** `clip` with its fades shortened so that fade-in + fade-out never exceeds its length. */
export function clampFades(clip: Clip): Clip {
  if (clip.content.type !== "audio") return clip;
  const fadeIn = clamp(clip.content.fade_in, 0, clip.length);
  const fadeOut = clamp(clip.content.fade_out, 0, clip.length - fadeIn);
  if (fadeIn === clip.content.fade_in && fadeOut === clip.content.fade_out) return clip;
  return { ...clip, content: { ...clip.content, fade_in: fadeIn, fade_out: fadeOut } };
}

/** A gain in dB inside the allowed range (not a number reads as unity). */
export function clampGain(db: number): number {
  if (Number.isNaN(db)) return 0;
  return clamp(db, MIN_GAIN_DB, MAX_GAIN_DB);
}

const replace = (clip: Clip): EditCommand => ({ command: "replace_clip", clip });

/** Media frames (at the media's own rate) that `beats` from `at` cover. */
export function framesBetween(
  tempo: TempoMap,
  at: number,
  beats: number,
  media: MediaBounds,
): number {
  return (tempo.beatsToSeconds(at + beats) - tempo.beatsToSeconds(at)) * media.sample_rate;
}

/**
 * Drags the left edge of `clip` to beat `target`. The start moves, the length shrinks (or grows)
 * by the same amount and an audio clip's `source_offset` advances by the media frames the moved
 * span covers, so the audio stays where it was on the timeline. The edge stops at the clip's end
 * (minus `MIN_CLIP_BEATS`), at beat 0 and, for audio, where `source_offset` would go below 0.
 * A MIDI clip keeps its notes where they are on the timeline: their clip-relative times shift
 * by the edge's travel; notes that end at or before the new start are removed and a note that
 * straddles it is shortened to start at 0 (controllers before it are removed too).
 */
export function trimLeft(clip: Clip, target: number, tempo: TempoMap, media?: MediaBounds): Clip {
  const end = clip.start + clip.length;
  let low = 0;
  const content = clip.content;
  if (content.type === "audio") {
    if (!media) return clip;
    const back = content.source_offset / Math.max(1, media.sample_rate);
    low = Math.max(0, tempo.secondsToBeats(Math.max(0, tempo.beatsToSeconds(clip.start) - back)));
  }
  if (!(end - MIN_CLIP_BEATS >= low)) return clip;
  const start = clamp(target, low, end - MIN_CLIP_BEATS);
  if (start === clip.start) return clip;
  const moved: Clip = { ...clip, start, length: end - start };
  if (content.type === "midi") return rebaseMidi(moved, start - clip.start);
  if (content.type !== "audio" || !media) return clampFades(moved);
  const frames = content.source_offset + framesBetween(tempo, clip.start, start - clip.start, media);
  const offset = clamp(Math.round(frames), 0, Math.max(0, media.frames - 1));
  return clampFades({ ...moved, content: { ...content, source_offset: offset } });
}

/** `clip` (already given its new start and length) with its notes moved back by `delta` beats. */
function rebaseMidi(clip: Clip, delta: number): Clip {
  if (clip.content.type !== "midi") return clip;
  const notes = clip.content.notes.flatMap((note) => {
    const start = note.start - delta;
    const end = start + note.length;
    if (end <= 0) return [];
    return [start < 0 ? { ...note, start: 0, length: end } : { ...note, start }];
  });
  const controllers = clip.content.controllers
    .map((c) => ({ ...c, beat: c.beat - delta }))
    .filter((c) => c.beat >= 0);
  return { ...clip, content: { ...clip.content, notes, controllers } };
}

/**
 * Drags the right edge of `clip` to beat `target`: only the length changes. It stays above
 * `MIN_CLIP_BEATS` and, for audio, within the media (`source_offset` plus the length in frames
 * never passes the media's last frame).
 */
export function trimRight(clip: Clip, target: number, tempo: TempoMap, media?: MediaBounds): Clip {
  const low = clip.start + MIN_CLIP_BEATS;
  let high = Number.POSITIVE_INFINITY;
  if (clip.content.type === "audio") {
    if (!media) return clip;
    const seconds = (media.frames - clip.content.source_offset) / Math.max(1, media.sample_rate);
    high = tempo.secondsToBeats(tempo.beatsToSeconds(clip.start) + Math.max(0, seconds));
  }
  if (!(high >= low)) return clip;
  const length = clamp(target, low, high) - clip.start;
  return length === clip.length ? clip : clampFades({ ...clip, length });
}

/** Drags the fade-in handle to beat `target`; the fade-out keeps its length and fades never overlap. */
export function setFadeIn(clip: Clip, target: number): Clip {
  if (clip.content.type !== "audio") return clip;
  const fadeIn = clamp(target - clip.start, 0, clip.length - clip.content.fade_out);
  return { ...clip, content: { ...clip.content, fade_in: fadeIn } };
}

/** Drags the fade-out handle to beat `target`; the fade-in keeps its length and fades never overlap. */
export function setFadeOut(clip: Clip, target: number): Clip {
  if (clip.content.type !== "audio") return clip;
  const fadeOut = clamp(clip.start + clip.length - target, 0, clip.length - clip.content.fade_in);
  return { ...clip, content: { ...clip.content, fade_out: fadeOut } };
}

/** The clip after dragging the handle `mode` to beat `target`. */
export function adjustClip(
  clip: Clip,
  mode: AdjustMode,
  target: number,
  tempo: TempoMap,
  media?: MediaBounds,
): Clip {
  switch (mode) {
    case "trim-left":
      return trimLeft(clip, target, tempo, media);
    case "trim-right":
      return trimRight(clip, target, tempo, media);
    case "fade-in":
      return setFadeIn(clip, target);
    case "fade-out":
      return setFadeOut(clip, target);
  }
}

/** The beat a handle sits at before it is dragged. */
export function handleBeat(clip: Clip, mode: AdjustMode): number {
  const fades = clip.content.type === "audio" ? clip.content : { fade_in: 0, fade_out: 0 };
  switch (mode) {
    case "trim-left":
      return clip.start;
    case "trim-right":
      return clip.start + clip.length;
    case "fade-in":
      return clip.start + fades.fade_in;
    case "fade-out":
      return clip.start + clip.length - fades.fade_out;
  }
}

/**
 * Where a handle lands after a drag of `dx` pixels: its starting beat plus the travel, snapped
 * to the grid unless `bypass` (Alt).
 */
export function dragTarget(
  clip: Clip,
  mode: AdjustMode,
  dx: number,
  view: View,
  signatures: SignatureMap,
  resolution: SnapResolution,
  bypass = false,
): number {
  return snapBeat(handleBeat(clip, mode) + dx / view.pxPerBeat, resolution, signatures, bypass);
}

/** Whether `a` and `b` are the same clip, content included. */
export const sameClip = (a: Clip, b: Clip): boolean => JSON.stringify(a) === JSON.stringify(b);

/** Applies a finished handle drag: one `ReplaceClip`, one undo step. False when nothing changed. */
export async function commitAdjust(
  api: ArrangeApi,
  original: Clip,
  adjusted: Clip,
  mode: AdjustMode,
): Promise<boolean> {
  if (sameClip(original, adjusted)) return false;
  const label = mode === "fade-in" || mode === "fade-out" ? "Fade clip" : "Trim clip";
  await inTransaction(api, label, () => api.edit(replace(adjusted)));
  return true;
}

/**
 * Splits `clip` at beat `at` into the two clips that together play the same thing, or null when
 * `at` is not inside the clip (at least `MIN_CLIP_BEATS` from both ends) or the clip cannot be
 * split (a pattern clip; an audio clip without its media).
 *
 * The left part keeps the clip's id and start; the right part has id `rightId` and starts at
 * `at`. An audio right part's `source_offset` advances by the media frames the left part covers;
 * the fade-in stays with the left part and the fade-out with the right (each shortened to fit).
 * A MIDI right part takes the notes and controllers at or after the split, rebased to its own
 * start; the left keeps the rest (a note running past the split is cut at the clip end).
 */
export function splitClip(
  clip: Clip,
  at: number,
  tempo: TempoMap,
  rightId: number,
  media?: MediaBounds,
): { left: Clip; right: Clip } | null {
  const end = clip.start + clip.length;
  if (at - clip.start < MIN_CLIP_BEATS || end - at < MIN_CLIP_BEATS) return null;
  const leftLength = at - clip.start;
  const content = clip.content;
  const left: Clip = { ...clip, length: leftLength };
  // The right part is a plain clip: only the left keeps the clip's take.
  const right: Clip = { ...clip, id: rightId, start: at, length: end - at, take: null };
  if (content.type === "audio") {
    if (!media) return null;
    const advance = framesBetween(tempo, clip.start, leftLength, media);
    left.content = { ...content, fade_out: 0 };
    right.content = {
      ...content,
      source_offset: Math.round(content.source_offset + advance),
      fade_in: 0,
    };
    return { left: clampFades(left), right: clampFades(right) };
  }
  if (content.type === "midi") {
    left.content = {
      ...content,
      notes: content.notes.filter((n) => n.start < leftLength),
      controllers: content.controllers.filter((c) => c.beat < leftLength),
    };
    right.content = {
      ...content,
      notes: content.notes
        .filter((n) => n.start >= leftLength)
        .map((n) => ({ ...n, start: n.start - leftLength })),
      controllers: content.controllers
        .filter((c) => c.beat >= leftLength)
        .map((c) => ({ ...c, beat: c.beat - leftLength })),
    };
    return { left, right };
  }
  return null;
}

/** The clips among `clips` that a split at `at` would cut. */
export function splittable(
  clips: readonly Clip[],
  at: number,
  tempo: TempoMap,
  mediaOf: (clip: Clip) => MediaBounds | undefined,
): Clip[] {
  return clips.filter((clip) => splitClip(clip, at, tempo, 0, mediaOf(clip)) !== null);
}

/**
 * Splits every clip of `clips` that spans `at`, in one transaction: each is replaced by its left
 * part and its right part is added with a fresh id. Resolves with the new (right) clips' ids.
 */
export async function splitClips(
  api: ArrangeApi,
  clips: readonly Clip[],
  at: number,
  tempo: TempoMap,
  mediaOf: (clip: Clip) => MediaBounds | undefined,
): Promise<number[]> {
  const targets = splittable(clips, at, tempo, mediaOf);
  if (targets.length === 0) return [];
  const ids = await api.allocIds(targets.length);
  if (ids.length < targets.length) throw new Error("not enough ids were reserved");
  await inTransaction(api, targets.length === 1 ? "Split clip" : "Split clips", async () => {
    for (const [i, clip] of targets.entries()) {
      const parts = splitClip(clip, at, tempo, ids[i]!, mediaOf(clip));
      if (!parts) continue;
      await api.edit(replace(parts.left));
      await api.edit({ command: "add_clip", clip: parts.right });
    }
  });
  return ids.slice(0, targets.length);
}

/**
 * Copies of `clips` (with ids `ids`, in start order) placed after the last of them: the group
 * keeps its layout and its earliest clip starts where the latest-ending clip ends.
 */
export function duplicates(clips: readonly Clip[], ids: readonly number[]): Clip[] {
  if (clips.length === 0) return [];
  const ordered = [...clips].sort((a, b) => a.start - b.start || a.id - b.id);
  const first = ordered[0]!.start;
  const last = Math.max(...ordered.map((c) => c.start + c.length));
  return ordered.map((clip, i) => ({
    ...structuredClone(clip),
    id: ids[i]!,
    start: clip.start - first + last,
    // A copy is not another take of the recording.
    take: null,
  }));
}

/** Duplicates the clips in one transaction; resolves with the new clips' ids. */
export async function duplicateClips(api: ArrangeApi, clips: readonly Clip[]): Promise<number[]> {
  if (clips.length === 0) return [];
  const ids = await api.allocIds(clips.length);
  if (ids.length < clips.length) throw new Error("not enough ids were reserved");
  const copies = duplicates(clips, ids);
  await inTransaction(api, clips.length === 1 ? "Duplicate clip" : "Duplicate clips", async () => {
    for (const clip of copies) await api.edit({ command: "add_clip", clip });
  });
  return copies.map((c) => c.id);
}

/** Sets the gain (dB, clamped) of the audio clips among `clips`; false when nothing changed. */
export async function setClipsGain(
  api: ArrangeApi,
  clips: readonly Clip[],
  db: number,
): Promise<boolean> {
  const gain = clampGain(db);
  const changed = clips.filter((c) => c.content.type === "audio" && c.content.gain_db !== gain);
  if (changed.length === 0) return false;
  await inTransaction(api, "Clip gain", async () => {
    for (const clip of changed) {
      if (clip.content.type !== "audio") continue;
      await api.edit(replace({ ...clip, content: { ...clip.content, gain_db: gain } }));
    }
  });
  return true;
}

/** Mutes the clips, or unmutes them when all are already muted; false when there are none. */
export async function toggleClipsMuted(api: ArrangeApi, clips: readonly Clip[]): Promise<boolean> {
  if (clips.length === 0) return false;
  const muted = !clips.every((c) => c.muted);
  await inTransaction(api, muted ? "Mute clips" : "Unmute clips", async () => {
    for (const clip of clips) if (clip.muted !== muted) await api.edit(replace({ ...clip, muted }));
  });
  return true;
}
