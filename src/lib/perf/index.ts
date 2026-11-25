import { runScrollBenchmark, type ScrollBenchmarkResult } from "./scroll";

export type { ScrollBenchmarkResult } from "./scroll";

declare global {
  interface Window {
    /** Development builds only: performance probes for manual frame-budget checks. */
    __owPerf?: { scrollBenchmark(): Promise<ScrollBenchmarkResult> };
  }
}

/**
 * Exposes `window.__owPerf.scrollBenchmark()` in development builds (nothing in a release build).
 * `step` moves the arrangement's scroll position for the given elapsed milliseconds. Returns a
 * function that removes the hook.
 */
export function installPerfHook(step: (elapsedMs: number) => void): () => void {
  if (!import.meta.env.DEV || typeof window === "undefined") return () => {};
  const hook = {
    scrollBenchmark: () =>
      runScrollBenchmark({ requestFrame: (cb) => requestAnimationFrame(cb) }, step),
  };
  window.__owPerf = hook;
  return () => {
    if (window.__owPerf === hook) delete window.__owPerf;
  };
}
