import type { Controller } from "../ipc/types";
import { type RollView, snapNearest } from "./grid";

const EPS = 1e-9;

/** Pixels around a point that still grab it (for erasing). */
export const POINT_GRAB = 7;

/** Common controllers for the picker; any 0 to 127 can also be typed. */
export const CC_PRESETS: readonly { controller: number; name: string }[] = [
  { controller: 1, name: "Mod wheel" },
  { controller: 2, name: "Breath" },
  { controller: 7, name: "Volume" },
  { controller: 10, name: "Pan" },
  { controller: 11, name: "Expression" },
  { controller: 64, name: "Sustain" },
  { controller: 71, name: "Resonance" },
  { controller: 74, name: "Cutoff" },
];

/** The mod wheel, the lane's default controller. */
export const DEFAULT_CC = 1;

/** "CC 1 Mod wheel", or "CC 20" for a controller without a preset name. */
export function ccLabel(controller: number): string {
  const preset = CC_PRESETS.find((p) => p.controller === controller);
  return preset ? `CC ${controller} ${preset.name}` : `CC ${controller}`;
}

/** `value` as a controller number, 0 to 127 (whole numbers; anything else falls back to the default). */
export const clampController = (value: number): number =>
  Number.isFinite(value) ? Math.min(127, Math.max(0, Math.round(value))) : DEFAULT_CC;

/** The events of one controller, ordered by beat. */
export const ccEvents = (controllers: readonly Controller[], controller: number): Controller[] =>
  controllers.filter((c) => c.controller === controller).sort((a, b) => a.beat - b.beat);

/** The value (0 to 127) at pixel `y` of a lane `height` pixels tall. */
export function ccValueAtY(y: number, height: number): number {
  if (!(height > 0)) return 0;
  return Math.min(127, Math.max(0, Math.round(127 * (1 - y / height))));
}

/** An event's centre in a lane `height` pixels tall. */
export const ccPoint = (view: RollView, event: Pick<Controller, "beat" | "value">, height: number) => ({
  x: event.beat * view.pxPerBeat,
  y: height - (event.value / 127) * height,
});

/** The events in beat order (stable for equal beats), the form the clip stores. */
export const sortControllers = (controllers: readonly Controller[]): Controller[] =>
  [...controllers].sort((a, b) => a.beat - b.beat);

/** One point of a drawn stroke, in clip beats. */
export interface StrokePoint {
  beat: number;
  value: number;
}

const stepOf = (beat: number, grid: number) => Math.round(snapNearest(beat, grid) / grid);

/**
 * Draws the stroke from `from` (null: just `to`) to `to` into the events of `controller`: one
 * event on every grid line the stroke crosses, valued by straight-line interpolation. An event
 * already on a line is replaced; events of other controllers and off-grid events are untouched.
 * Lines outside the clip (0 up to `clipLength`) are skipped.
 */
export function paintStroke(
  controllers: readonly Controller[],
  controller: number,
  from: StrokePoint | null,
  to: StrokePoint,
  grid: number,
  clipLength: number,
): Controller[] {
  if (!(grid > 0)) return [...controllers];
  const fromStep = stepOf(from?.beat ?? to.beat, grid);
  const toStep = stepOf(to.beat, grid);
  const first = Math.min(fromStep, toStep);
  const last = Math.max(fromStep, toStep);
  const start = from?.value ?? to.value;
  const drawn = new Map<number, number>();
  for (let step = first; step <= last; step++) {
    const beat = step * grid;
    if (beat < -EPS || beat >= clipLength - EPS) continue;
    const t = fromStep === toStep ? 1 : (step - fromStep) / (toStep - fromStep);
    drawn.set(step, Math.min(127, Math.max(0, Math.round(start + (to.value - start) * t))));
  }
  const kept = controllers.filter(
    (c) =>
      c.controller !== controller ||
      Math.abs(c.beat - snapNearest(c.beat, grid)) > EPS ||
      !drawn.has(stepOf(c.beat, grid)),
  );
  const added: Controller[] = [...drawn].map(([step, value]) => ({
    beat: Math.max(0, step * grid),
    controller,
    value,
  }));
  return sortControllers([...kept, ...added]);
}

/**
 * The event list without the event of `controller` nearest to pixel `x` (within
 * [`POINT_GRAB`] pixels), or null when none is that close.
 */
export function eraseNear(
  controllers: readonly Controller[],
  controller: number,
  view: RollView,
  x: number,
): Controller[] | null {
  let best = -1;
  let distance = POINT_GRAB;
  controllers.forEach((c, i) => {
    if (c.controller !== controller) return;
    const d = Math.abs(c.beat * view.pxPerBeat - x);
    if (d <= distance) {
      distance = d;
      best = i;
    }
  });
  return best < 0 ? null : controllers.filter((_, i) => i !== best);
}

/** The events without those of `controller`. */
export const clearController = (controllers: readonly Controller[], controller: number): Controller[] =>
  controllers.filter((c) => c.controller !== controller);
