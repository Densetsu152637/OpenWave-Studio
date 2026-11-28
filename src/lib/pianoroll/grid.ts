import type { SignatureMap } from "../arrange/time";
import type { Note } from "../ipc/types";

/**
 * Geometry of the piano roll. Beats here are relative to the clip's start; the roll shows all 128
 * keys (key 127 on top) and scrolls natively, so a position is just beats and keys times the
 * pixel sizes of a [`RollView`].
 */
export interface RollView {
  /** Pixels per beat. */
  pxPerBeat: number;
  /** Pixels per key row. */
  pxPerKey: number;
}

export const KEYS = 128;
export const MIN_PX_PER_BEAT = 8;
export const MAX_PX_PER_BEAT = 400;
export const DEFAULT_ROLL_VIEW: RollView = { pxPerBeat: 80, pxPerKey: 14 };

/** A snap grid: its label and length in beats. */
export interface GridOption {
  label: string;
  beats: number;
}

export const GRID_OPTIONS: readonly GridOption[] = [
  { label: "1/32", beats: 0.125 },
  { label: "1/16", beats: 0.25 },
  { label: "1/8", beats: 0.5 },
  { label: "1/4", beats: 1 },
  { label: "1/2", beats: 2 },
  { label: "1 bar", beats: 4 },
];
export const DEFAULT_GRID = 0.25;

/** The note's rectangle in the roll, in pixels. */
export function noteRect(view: RollView, note: Pick<Note, "start" | "length" | "key">) {
  return {
    x: note.start * view.pxPerBeat,
    y: (KEYS - 1 - note.key) * view.pxPerKey,
    width: note.length * view.pxPerBeat,
    height: view.pxPerKey,
  };
}

/** Total size of the roll for a clip `length` beats long. */
export const rollSize = (view: RollView, length: number) => ({
  width: length * view.pxPerBeat,
  height: KEYS * view.pxPerKey,
});

/** The beat (clip-relative, fractional) and key (0 to 127) under pixel `x`, `y`. */
export function pointToCell(view: RollView, x: number, y: number) {
  const key = KEYS - 1 - Math.floor(y / view.pxPerKey);
  return { beat: x / view.pxPerBeat, key: Math.min(KEYS - 1, Math.max(0, key)) };
}

/** `beat` rounded down to the grid. */
export const snapDown = (beat: number, grid: number): number =>
  Math.floor(beat / grid + 1e-9) * grid;

/** `beat` rounded to the nearest grid line (halves round up). */
export const snapNearest = (beat: number, grid: number): number =>
  Math.floor(beat / grid + 0.5 + 1e-9) * grid;

/** Pixels at a note's right edge that grab it for resizing. */
export const EDGE_PX = 6;

export type NotePart = "body" | "start" | "end";

/**
 * The topmost note under pixel `x`, `y` (the last in the list wins an overlap) and which part:
 * its left edge (resize the start), its right edge (resize the end) or its body (move).
 */
export function hitTest(
  notes: readonly Note[],
  view: RollView,
  x: number,
  y: number,
): { index: number; part: NotePart } | null {
  for (let index = notes.length - 1; index >= 0; index--) {
    const rect = noteRect(view, notes[index]!);
    if (x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height) {
      const edge = Math.min(EDGE_PX, rect.width / 3);
      const part = x >= rect.x + rect.width - edge ? "end" : x < rect.x + edge ? "start" : "body";
      return { index, part };
    }
  }
  return null;
}

export interface GridLine {
  beat: number;
  x: number;
  kind: "bar" | "beat" | "grid";
}

/** Most lines drawn; beyond this the finer ones are dropped. */
const MAX_LINES = 4000;

/**
 * The vertical lines of a clip `length` beats long: one per `grid` beats, marked as bar lines
 * and beat lines by the project's time signature (`start` is the clip's position on the
 * timeline, so bar lines fall where the arrangement's bars do).
 */
export function gridLines(
  view: RollView,
  start: number,
  length: number,
  grid: number,
  signatures: SignatureMap,
): GridLine[] {
  const step = grid > 0 ? Math.max(grid, length / MAX_LINES) : 1;
  const lines: GridLine[] = [];
  for (let local = 0; local <= length + 1e-9; local += step) {
    const bar = signatures.barAt(start + local);
    const intoBar = start + local - bar.start;
    const beatLength = bar.length / bar.beats;
    let kind: GridLine["kind"] = "grid";
    if (Math.abs(intoBar) < 1e-9 || Math.abs(intoBar - bar.length) < 1e-9) kind = "bar";
    else if (Math.abs(intoBar / beatLength - Math.round(intoBar / beatLength)) < 1e-9) kind = "beat";
    lines.push({ beat: local, x: local * view.pxPerBeat, kind });
  }
  return lines;
}

const NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/** Scientific pitch name of a MIDI key: 60 is "C4". */
export function keyName(key: number): string {
  return `${NAMES[((key % 12) + 12) % 12]}${Math.floor(key / 12) - 1}`;
}

/** True for the keys that are black on a piano. */
export function isBlackKey(key: number): boolean {
  return [1, 3, 6, 8, 10].includes(((key % 12) + 12) % 12);
}

/** The scroll offset (pixels from the top) that puts the middle of the notes' key range in view. */
export function initialScrollTop(
  notes: readonly Pick<Note, "key">[],
  view: RollView,
  viewportHeight: number,
): number {
  const keys = notes.map((n) => n.key);
  const middle = keys.length > 0 ? (Math.min(...keys) + Math.max(...keys)) / 2 : 60;
  const y = (KEYS - 1 - middle) * view.pxPerKey + view.pxPerKey / 2;
  return Math.max(0, Math.min(KEYS * view.pxPerKey - viewportHeight, y - viewportHeight / 2));
}

/** Zooms the roll horizontally by `factor`, within limits. */
export function zoomRoll(view: RollView, factor: number): RollView {
  return {
    ...view,
    pxPerBeat: Math.min(MAX_PX_PER_BEAT, Math.max(MIN_PX_PER_BEAT, view.pxPerBeat * factor)),
  };
}
