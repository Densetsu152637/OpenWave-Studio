import type { TempoMap } from "./time";
import { beatToX, xToBeat, type View } from "./view";

/** `mm:ss.mmm`; minutes keep counting past 59. Negative input shows as zero. */
export function formatTime(seconds: number): string {
  const total = Math.max(0, Math.round((Number.isFinite(seconds) ? seconds : 0) * 1000));
  const ms = total % 1000;
  const secs = Math.floor(total / 1000) % 60;
  const mins = Math.floor(total / 60000);
  const pad = (value: number, size: number) => String(value).padStart(size, "0");
  return `${pad(mins, 2)}:${pad(secs, 2)}.${pad(ms, 3)}`;
}

export interface TimeMark {
  x: number;
  beat: number;
  seconds: number;
  /** A labelled (major) mark; the others are minor ticks. */
  major: boolean;
  label: string | null;
}

/** Steps, in seconds, a time ruler's labelled marks may be apart. */
const STEPS = [
  0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600,
  1800, 3600,
];
/** Pixels a time label needs. */
const LABEL_PX = 72;
/** Minor ticks (five per major step) appear once they are at least this far apart. */
const MIN_TICK_PX = 8;

/** The labelled marks (and minor ticks when zoomed in) of a time ruler `width` pixels wide. */
export function timeMarks(view: View, width: number, tempo: TempoMap): TimeMark[] {
  const startSeconds = tempo.beatsToSeconds(view.originBeat);
  const endSeconds = tempo.beatsToSeconds(xToBeat(view, width));
  const span = endSeconds - startSeconds;
  if (!(span > 0) || width <= 0) return [];
  const pxPerSecond = width / span;
  const step = STEPS.find((s) => s * pxPerSecond >= LABEL_PX) ?? STEPS[STEPS.length - 1]!;
  const minor = step / 5;
  const unit = minor * pxPerSecond >= MIN_TICK_PX ? minor : step;
  const marks: TimeMark[] = [];
  for (let i = Math.max(0, Math.floor(startSeconds / unit)); i * unit <= endSeconds + 1e-9; i++) {
    const seconds = i * unit;
    const major = Math.abs(seconds / step - Math.round(seconds / step)) < 1e-6;
    const beat = tempo.secondsToBeats(seconds);
    const x = beatToX(view, beat);
    if (x < -1) continue;
    marks.push({ x, beat, seconds, major, label: major ? formatTime(seconds) : null });
  }
  return marks;
}
