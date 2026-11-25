import type { SignatureMap } from "./time";
import type { View } from "./view";

/** The snap grid: off, to the bar, or to a beat (a quarter note) or a fraction of one. */
export type SnapResolution = "off" | "bar" | "1" | "1/2" | "1/4" | "1/8" | "1/16";

export interface SnapOption {
  id: SnapResolution;
  label: string;
  /** Grid step in beats; null for off and for bars (whose length varies). */
  beats: number | null;
}

export const SNAP_OPTIONS: readonly SnapOption[] = [
  { id: "off", label: "Off", beats: null },
  { id: "bar", label: "Bar", beats: null },
  { id: "1", label: "1 beat", beats: 1 },
  { id: "1/2", label: "1/2 beat", beats: 0.5 },
  { id: "1/4", label: "1/4 beat", beats: 0.25 },
  { id: "1/8", label: "1/8 beat", beats: 0.125 },
  { id: "1/16", label: "1/16 beat", beats: 0.0625 },
];

export const DEFAULT_SNAP: SnapResolution = "bar";

export function isSnapResolution(value: string): value is SnapResolution {
  return SNAP_OPTIONS.some((option) => option.id === value);
}

/**
 * `beat` moved to the nearest grid line, never before the timeline's start. `bypass` (Alt held)
 * and `off` leave it where it is.
 */
export function snapBeat(
  beat: number,
  resolution: SnapResolution,
  signatures: SignatureMap,
  bypass = false,
): number {
  if (resolution === "off" || bypass) return Math.max(0, beat);
  if (resolution === "bar") return Math.max(0, signatures.nearestBarStart(beat));
  const step = SNAP_OPTIONS.find((option) => option.id === resolution)?.beats ?? 1;
  return Math.max(0, Math.round(beat / step) * step);
}

/**
 * The beat shift of a drag of `dx` pixels, taken from the dragged clip's own start
 * `originStart`: that clip lands on the grid and every other selected clip moves by the same
 * shift. The shift never takes the earliest clip (at `minStart`) before beat 0.
 */
export function dragDelta(
  originStart: number,
  minStart: number,
  dx: number,
  view: View,
  signatures: SignatureMap,
  resolution: SnapResolution,
  bypass = false,
): number {
  const target = snapBeat(originStart + dx / view.pxPerBeat, resolution, signatures, bypass);
  return Math.max(target - originStart, -minStart);
}
