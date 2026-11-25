import { describe, expect, it } from "vitest";
import { MovingTracker, START_GRACE_MS, STALL_MS, derivePlaying } from "./playback";

describe("engine moving state", () => {
  it("moves while the position advances and stops when it holds", () => {
    const tracker = new MovingTracker();
    expect(tracker.moving(0)).toBe(false);
    tracker.update(1000, 0);
    tracker.update(1512, 10);
    expect(tracker.moving(20)).toBe(true);
    tracker.update(1512, 100);
    tracker.update(1512, 10 + STALL_MS);
    expect(tracker.moving(10 + STALL_MS)).toBe(false);
  });

  it("a locate while stopped shows as moving for one stall window only", () => {
    const tracker = new MovingTracker();
    tracker.update(0, 0);
    tracker.update(48_000, 1000);
    expect(tracker.moving(1010)).toBe(true);
    expect(tracker.moving(1000 + STALL_MS)).toBe(false);
  });

  it("derives playing from the engine, with a grace after the UI's own Play", () => {
    expect(derivePlaying(true, false, -1e9, 5000)).toBe(true); // an agent started it
    expect(derivePlaying(false, true, -1e9, 5000)).toBe(false); // stopped at the end or by an agent
    expect(derivePlaying(false, true, 5000, 5000 + START_GRACE_MS - 1)).toBe(true);
    expect(derivePlaying(false, true, 5000, 5000 + START_GRACE_MS)).toBe(false);
    expect(derivePlaying(false, false, 5000, 5001)).toBe(false);
  });
});
