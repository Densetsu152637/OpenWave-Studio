import { describe, expect, it } from "vitest";
import {
  DOCK_PLAIN_HEIGHT,
  MIN_ARRANGEMENT,
  MIN_GRID,
  ROLL_CHROME,
  clampDock,
  defaultDock,
  dockHeight,
  dockMax,
  dockMin,
  parseDockHeight,
} from "./dock";

describe("dock sizing", () => {
  it("keeps the grid and the arrangement visible when clamping", () => {
    expect(dockMin(true)).toBe(MIN_GRID + ROLL_CHROME);
    expect(clampDock(10, 900, true)).toBe(dockMin(true));
    expect(clampDock(5000, 900, true)).toBe(900 - MIN_ARRANGEMENT);
    expect(clampDock(500, 900, true)).toBe(500);
  });

  it("falls back to the minimum when the space is tiny or the value is bad", () => {
    expect(dockMax(200, true)).toBe(dockMin(true));
    expect(clampDock(300, 200, true)).toBe(dockMin(true));
    expect(clampDock(Number.NaN, 900, false)).toBe(dockMin(false));
  });

  it("defaults to 45% of the space, at least 420, with a roll open", () => {
    expect(defaultDock(1200, true)).toBe(540);
    expect(defaultDock(800, true)).toBe(460);
    expect(defaultDock(1100, true)).toBe(495);
    expect(defaultDock(500, true)).toBe(460);
    expect(defaultDock(900, false)).toBe(DOCK_PLAIN_HEIGHT);
  });

  it("uses a saved height clamped to the current space", () => {
    expect(dockHeight(null, 1200, true)).toBe(540);
    expect(dockHeight(600, 1200, true)).toBe(600);
    expect(dockHeight(900, 700, true)).toBe(580);
  });

  it("parses stored heights", () => {
    expect(parseDockHeight(null)).toBeNull();
    expect(parseDockHeight("")).toBeNull();
    expect(parseDockHeight("abc")).toBeNull();
    expect(parseDockHeight("-4")).toBeNull();
    expect(parseDockHeight("431.6")).toBe(432);
  });
});
