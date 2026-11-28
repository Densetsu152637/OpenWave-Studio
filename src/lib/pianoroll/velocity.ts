import type { Note } from "../ipc/types";
import type { RollView } from "./grid";

/** Width of a velocity bar in pixels. */
export const BAR_WIDTH = 7;
/** Pixels below a bar's top that still grab it (a bar can be grabbed a little above its top too). */
const GRAB = 10;

/** A velocity bar's rectangle in a lane `height` pixels tall: full height is velocity 127. */
export function barRect(view: RollView, note: Pick<Note, "start" | "velocity">, height: number) {
  const h = Math.max(1, (note.velocity / 127) * height);
  return { x: note.start * view.pxPerBeat, y: height - h, width: BAR_WIDTH, height: h };
}

/** The velocity (1 to 127) at pixel `y` of a lane `height` pixels tall. */
export function velocityAtY(y: number, height: number): number {
  if (!(height > 0)) return 1;
  return Math.min(127, Math.max(1, Math.round(127 * (1 - y / height))));
}

/**
 * The bar under pixel `x`, `y`. Where chords stack bars on one x, the one whose top is nearest
 * to `y` wins, so each bar of a chord can be grabbed at its own height.
 */
export function hitBar(
  notes: readonly Pick<Note, "start" | "velocity">[],
  view: RollView,
  x: number,
  y: number,
  height: number,
): number | null {
  let best: number | null = null;
  let distance = Infinity;
  notes.forEach((note, index) => {
    const rect = barRect(view, note, height);
    if (x < rect.x - 1 || x > rect.x + rect.width + 1) return;
    if (y < rect.y - GRAB) return;
    const d = Math.abs(y - rect.y);
    if (d < distance) {
      distance = d;
      best = index;
    }
  });
  return best;
}
