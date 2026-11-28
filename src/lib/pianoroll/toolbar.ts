import type { LegatoMode, MidiEdit } from "../ipc/types";
import { DEFAULT_CC } from "./cc";
import { DEFAULT_GRID, GRID_OPTIONS } from "./grid";
import { SCALES } from "./scale";

/** Settings of the toolbar's note operations, as the roll keeps them. */
export interface OpSettings {
  /** Quantize strength in percent, 0 to 100. */
  strength: number;
  /** Quantize note lengths too. */
  lengths: boolean;
  legato: LegatoMode;
  /** Humanize: largest start shift in beats, and largest velocity change. */
  timing: number;
  velocity: number;
  /** Humanize seed: the same seed gives the same result. */
  seed: number;
  /** Velocity set to this value. */
  setTo: number;
  /** Velocity scale in percent. */
  scalePercent: number;
}

export const DEFAULT_OPS: OpSettings = {
  strength: 100,
  lengths: false,
  legato: "any_key",
  timing: 0.02,
  velocity: 8,
  seed: 1,
  setTo: 100,
  scalePercent: 90,
};

const clamp = (value: number, low: number, high: number, fallback: number) =>
  Number.isFinite(value) ? Math.min(high, Math.max(low, value)) : fallback;

/** Quantize to `grid` beats at the settings' strength (percent). */
export const quantizeEdit = (grid: number, ops: OpSettings): MidiEdit => ({
  op: "quantize",
  grid,
  strength: clamp(ops.strength, 0, 100, 100) / 100,
  lengths: ops.lengths,
});

export const legatoEdit = (ops: OpSettings): MidiEdit => ({ op: "legato", mode: ops.legato });

/** Humanize with the settings' seed, timing range (0 to 1 beat) and velocity range (0 to 127). */
export const humanizeEdit = (ops: OpSettings): MidiEdit => ({
  op: "humanize",
  seed: Math.max(0, Math.floor(clamp(ops.seed, 0, Number.MAX_SAFE_INTEGER, 1))),
  timing: clamp(ops.timing, 0, 1, 0.02),
  velocity: Math.round(clamp(ops.velocity, 0, 127, 8)),
});

export const transposeEdit = (semitones: number): MidiEdit => ({
  op: "transpose",
  semitones: Math.round(clamp(semitones, -127, 127, 0)),
});

export const velocitySetEdit = (ops: OpSettings): MidiEdit => ({
  op: "velocity_set",
  velocity: Math.round(clamp(ops.setTo, 1, 127, 100)),
});

export const velocityScaleEdit = (ops: OpSettings): MidiEdit => ({
  op: "velocity_scale",
  factor: clamp(ops.scalePercent, 1, 400, 100) / 100,
});

/** A new seed after `seed`, so repeated humanizing gives different results. */
export const nextSeed = (seed: number): number => (Math.floor(seed) % 1_000_000) + 1;

/** What the roll remembers between sessions (per viewer, in browser storage). */
export interface RollPrefs {
  grid: number;
  /** Scale root, a pitch class 0 to 11. */
  root: number;
  /** A scale id from `SCALES`. */
  scale: string;
  /** The CC lane's controller. */
  controller: number;
  tool: "draw" | "select";
}

export const DEFAULT_PREFS: RollPrefs = {
  grid: DEFAULT_GRID,
  root: 0,
  scale: "off",
  controller: DEFAULT_CC,
  tool: "draw",
};

/** Reads stored preferences, falling back to the defaults for anything missing or invalid. */
export function parsePrefs(raw: string | null): RollPrefs {
  if (!raw) return { ...DEFAULT_PREFS };
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return { ...DEFAULT_PREFS };
  }
  if (typeof value !== "object" || value === null) return { ...DEFAULT_PREFS };
  const v = value as Record<string, unknown>;
  const grid = GRID_OPTIONS.find((g) => g.beats === v.grid)?.beats ?? DEFAULT_PREFS.grid;
  const root = typeof v.root === "number" && Number.isInteger(v.root) && v.root >= 0 && v.root < 12 ? v.root : 0;
  const scale = SCALES.find((s) => s.id === v.scale)?.id ?? "off";
  const controller =
    typeof v.controller === "number" && Number.isInteger(v.controller) && v.controller >= 0 && v.controller <= 127
      ? v.controller
      : DEFAULT_CC;
  return { grid, root, scale, controller, tool: v.tool === "select" ? "select" : "draw" };
}
