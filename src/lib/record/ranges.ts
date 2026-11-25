import { inTransaction, type ArrangeApi } from "../arrange/edits";
import { snapBeat, type SnapResolution } from "../arrange/snap";
import type { SignatureMap } from "../arrange/time";
import type { BeatRange, ProjectSettings } from "../ipc/types";

/** Which range a gesture edits. */
export type RangeKind = "loop" | "punch";

/**
 * What a drag grabbed: nothing (it creates a new range), one end, or the body (moves the range).
 */
export type RangeGrab = "create" | "start" | "end" | "move";

/** Screen pixels within which a pointer is on a range end rather than its body. */
export const END_GRAB_PX = 6;

/** The part of the range under the pointer at pixel distances from its ends. */
export function grabAt(
  range: BeatRange | null,
  x: number,
  startX: number,
  endX: number,
): RangeGrab {
  if (!range) return "create";
  if (Math.abs(x - startX) <= END_GRAB_PX) return "start";
  if (Math.abs(x - endX) <= END_GRAB_PX) return "end";
  return x > startX && x < endX ? "move" : "create";
}

export interface RangeDrag {
  grab: RangeGrab;
  /** The range when the drag began; null when there was none. */
  origin: BeatRange | null;
  /** The beat under the pointer when it went down. */
  anchor: number;
}

export interface DragContext {
  signatures: SignatureMap;
  snap: SnapResolution;
  /** Alt held: skip the grid. */
  bypass: boolean;
}

/** The shortest range a drag may leave, in beats; shorter than this is a click, not a range. */
export const MIN_RANGE_BEATS = 1e-6;

/**
 * The range a drag shows with the pointer at `beat`, or null when it has none (a click, or a
 * creating drag that has not left its starting grid line). Ends land on the grid and the range
 * keeps a non-zero length; the body keeps its length and never starts before beat 0.
 */
export function dragRange(
  drag: RangeDrag,
  beat: number,
  context: DragContext,
): BeatRange | null {
  const at = (b: number) => snapBeat(b, context.snap, context.signatures, context.bypass);
  const { origin } = drag;
  switch (drag.grab) {
    case "create": {
      const a = at(drag.anchor);
      const b = at(beat);
      const range = { start: Math.min(a, b), end: Math.max(a, b) };
      return range.end - range.start > MIN_RANGE_BEATS ? range : null;
    }
    case "start": {
      if (!origin) return null;
      const start = Math.min(at(beat), origin.end - MIN_RANGE_BEATS * 2);
      return { start: Math.max(0, start), end: origin.end };
    }
    case "end": {
      if (!origin) return null;
      return { start: origin.start, end: Math.max(at(beat), origin.start + MIN_RANGE_BEATS * 2) };
    }
    case "move": {
      if (!origin) return null;
      const start = Math.max(0, at(origin.start + (beat - drag.anchor)));
      return { start, end: start + (origin.end - origin.start) };
    }
  }
}

/** The settings with one range replaced; a new punch range also turns punching on. */
export function withRange(
  settings: ProjectSettings,
  kind: RangeKind,
  range: BeatRange | null,
): ProjectSettings {
  if (kind === "loop") return { ...settings, loop_range: range };
  return {
    ...settings,
    punch_range: range,
    punch_enabled: range === null ? false : settings.punch_enabled || settings.punch_range === null,
  };
}

const sameRange = (a: BeatRange | null, b: BeatRange | null) =>
  a === b || (a !== null && b !== null && a.start === b.start && a.end === b.end);

/** Commits a range gesture as one undo step; a result equal to the current range does nothing. */
export async function commitRange(
  api: ArrangeApi,
  settings: ProjectSettings,
  kind: RangeKind,
  range: BeatRange | null,
): Promise<boolean> {
  const current = kind === "loop" ? settings.loop_range : settings.punch_range;
  if (sameRange(current, range)) return false;
  await inTransaction(api, kind === "loop" ? "Set loop range" : "Set punch range", () =>
    api.edit({ command: "set_settings", settings: withRange(settings, kind, range) }),
  );
  return true;
}

/** Longest count-in the model accepts, in bars. */
export const MAX_COUNT_IN = 16;
/** The count-in lengths the transport offers, in bars. */
export const COUNT_IN_CHOICES = [0, 1, 2, 4] as const;

export const countInLabel = (bars: number) => (bars === 0 ? "Off" : `${bars} bar${bars === 1 ? "" : "s"}`);

/** Count-in choices, plus the project's own value when an agent set one outside the list. */
export function countInOptions(current: number): number[] {
  return COUNT_IN_CHOICES.includes(current as 0) ? [...COUNT_IN_CHOICES] : [...COUNT_IN_CHOICES, current].sort((a, b) => a - b);
}

export async function setSettings(
  api: ArrangeApi,
  label: string,
  settings: ProjectSettings,
): Promise<void> {
  await inTransaction(api, label, () => api.edit({ command: "set_settings", settings }));
}

export const withCountIn = (settings: ProjectSettings, bars: number): ProjectSettings => ({
  ...settings,
  count_in_bars: Math.min(MAX_COUNT_IN, Math.max(0, Math.round(bars))),
});

export const withPunchEnabled = (settings: ProjectSettings, enabled: boolean): ProjectSettings => ({
  ...settings,
  punch_enabled: enabled,
});

/** A default loop (`bars` bars from the bar holding `beat`) for turning looping on from nothing. */
export function defaultLoop(beat: number, signatures: SignatureMap, bars = 4): BeatRange {
  const first = signatures.barAt(beat).index;
  return { start: signatures.barStart(first), end: signatures.barStart(first + bars) };
}
