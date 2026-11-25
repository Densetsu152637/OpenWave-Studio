import type { Clip } from "../ipc/types";
import { snapBeat, type SnapResolution } from "./snap";
import type { SignatureMap } from "./time";

/** The horizontal window onto the timeline. */
export interface View {
  /** Beat at the left edge. */
  originBeat: number;
  /** Pixels per beat. */
  pxPerBeat: number;
}

export const MIN_PX_PER_BEAT = 1;
export const MAX_PX_PER_BEAT = 600;
export const DEFAULT_VIEW: View = { originBeat: 0, pxPerBeat: 24 };

const clampZoom = (pxPerBeat: number) =>
  Math.min(MAX_PX_PER_BEAT, Math.max(MIN_PX_PER_BEAT, pxPerBeat));

export const beatToX = (view: View, beat: number): number =>
  (beat - view.originBeat) * view.pxPerBeat;

export const xToBeat = (view: View, x: number): number => view.originBeat + x / view.pxPerBeat;

/** Zooms by `factor` keeping the beat under pixel `anchorX` where it is. */
export function zoomAt(view: View, factor: number, anchorX: number): View {
  const pxPerBeat = clampZoom(view.pxPerBeat * factor);
  const anchorBeat = xToBeat(view, anchorX);
  return { pxPerBeat, originBeat: Math.max(0, anchorBeat - anchorX / pxPerBeat) };
}

/** Scrolls by `dx` pixels (positive moves later); the timeline starts at beat 0. */
export function scrollBy(view: View, dx: number): View {
  return { ...view, originBeat: Math.max(0, view.originBeat + dx / view.pxPerBeat) };
}

/** Where a clip sits, in pixels. */
export function clipRect(view: View, clip: Pick<Clip, "start" | "length">) {
  return { x: beatToX(view, clip.start), width: clip.length * view.pxPerBeat };
}

/**
 * The part of a clip inside a viewport `width` pixels wide, in pixels relative to the clip's
 * left edge; null when none of it is visible.
 */
export function clipVisibleSpan(
  view: View,
  clip: Pick<Clip, "start" | "length">,
  width: number,
): { left: number; right: number } | null {
  const rect = clipRect(view, clip);
  const left = Math.max(0, -rect.x);
  const right = Math.min(rect.width, width - rect.x);
  return right > left ? { left, right } : null;
}

/**
 * The start a dragged clip takes: its start plus the pointer travel of `dx` pixels, snapped to
 * the grid (the nearest bar unless told otherwise) and never before the timeline's start.
 */
export function dragClipStart(
  originStart: number,
  dx: number,
  view: View,
  signatures: SignatureMap,
  resolution: SnapResolution = "bar",
  bypass = false,
): number {
  return snapBeat(originStart + dx / view.pxPerBeat, resolution, signatures, bypass);
}

/** Where the last clip ends, in beats (0 for an empty project). */
export function projectEndBeat(clips: Iterable<Pick<Clip, "start" | "length">>): number {
  let end = 0;
  for (const clip of clips) end = Math.max(end, clip.start + clip.length);
  return end;
}

export interface RulerMark {
  x: number;
  beat: number;
  kind: "bar" | "beat";
  /** Bar number (1-based) when this mark carries a label. */
  label: string | null;
}

/** Pixels a bar label needs; bars closer than this share labels. */
const LABEL_PX = 48;
/** Bars closer than this are thinned out as lines too. */
const MIN_BAR_LINE_PX = 5;
/** Beat ticks appear once a beat is at least this wide. */
const MIN_BEAT_TICK_PX = 14;

/**
 * The bar lines (and beat ticks when zoomed in) visible in a ruler `width` pixels wide. Zoomed
 * out, only every 2^n-th bar gets a label, and below a few pixels per bar only those get a line.
 */
export function rulerMarks(view: View, width: number, signatures: SignatureMap): RulerMark[] {
  const marks: RulerMark[] = [];
  const endBeat = xToBeat(view, width);
  const first = signatures.barAt(view.originBeat);
  const barPx = first.length * view.pxPerBeat;
  let step = 1;
  while (barPx * step < LABEL_PX) step *= 2;
  const lineStep = barPx >= MIN_BAR_LINE_PX ? 1 : step;
  const showBeats = (first.length / first.beats) * view.pxPerBeat >= MIN_BEAT_TICK_PX;
  for (let bar = first.index - (first.index % lineStep); signatures.barStart(bar) <= endBeat; bar += lineStep) {
    const start = signatures.barStart(bar);
    const labelled = bar % step === 0;
    if (start >= view.originBeat - 1e-9) {
      marks.push({
        x: beatToX(view, start),
        beat: start,
        kind: "bar",
        label: labelled ? String(bar + 1) : null,
      });
    }
    if (showBeats && lineStep === 1) {
      const info = signatures.barAt(start);
      const beatLength = info.length / info.beats;
      for (let b = 1; b < info.beats; b++) {
        const beat = start + b * beatLength;
        if (beat >= view.originBeat) {
          marks.push({ x: beatToX(view, beat), beat, kind: "beat", label: null });
        }
      }
    }
  }
  return marks;
}

/** A track color (`0xRRGGBB`) as a CSS color. */
export const cssColor = (color: number): string =>
  `#${(color & 0xffffff).toString(16).padStart(6, "0")}`;
