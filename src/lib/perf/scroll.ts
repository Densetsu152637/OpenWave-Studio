/** What the dev-only scroll benchmark reports. */
export interface ScrollBenchmarkResult {
  /** Animation frames measured. */
  frames: number;
  /** 95th-percentile frame time in milliseconds. */
  p95Ms: number;
  /** Mean frame time in milliseconds. */
  meanMs: number;
}

/** How long the benchmark scrolls. */
export const BENCHMARK_MS = 10_000;

/** The `fraction` percentile (0 to 1) of `values`, nearest-rank; 0 for an empty list. */
export function percentile(values: readonly number[], fraction: number): number {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  const rank = Math.ceil(fraction * sorted.length);
  return sorted[Math.min(sorted.length - 1, Math.max(0, rank - 1))]!;
}

/** Frame statistics from the gaps between consecutive frame timestamps. */
export function summarizeFrames(timestamps: readonly number[]): ScrollBenchmarkResult {
  const times: number[] = [];
  for (let i = 1; i < timestamps.length; i++) times.push(timestamps[i]! - timestamps[i - 1]!);
  const mean = times.length === 0 ? 0 : times.reduce((a, b) => a + b, 0) / times.length;
  return { frames: times.length, p95Ms: percentile(times, 0.95), meanMs: mean };
}

/** The host a benchmark runs on; the real one is the browser, tests pass a fake. */
export interface FrameClock {
  requestFrame(callback: (timestamp: number) => void): void;
}

/**
 * Calls `step(elapsedMs)` once per animation frame for `durationMs` (it moves the scroll
 * position), and resolves with the frame-time statistics.
 */
export function runScrollBenchmark(
  clock: FrameClock,
  step: (elapsedMs: number) => void,
  durationMs = BENCHMARK_MS,
): Promise<ScrollBenchmarkResult> {
  return new Promise((resolve) => {
    const stamps: number[] = [];
    let first: number | null = null;
    const frame = (timestamp: number) => {
      first ??= timestamp;
      const elapsed = timestamp - first;
      stamps.push(timestamp);
      if (elapsed >= durationMs) {
        resolve(summarizeFrames(stamps));
        return;
      }
      step(elapsed);
      clock.requestFrame(frame);
    };
    clock.requestFrame(frame);
  });
}

/** A position that sweeps back and forth over `0..range` once per `periodMs`. */
export function pingPong(elapsedMs: number, range: number, periodMs: number): number {
  const phase = (elapsedMs % periodMs) / periodMs;
  return range * (phase < 0.5 ? phase * 2 : 2 - phase * 2);
}
