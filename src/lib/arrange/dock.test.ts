import { describe, expect, it } from "vitest";
import {
  DOCK_PLAIN_HEIGHT,
  FLOOR_GRID,
  MIN_GRID,
  clampDock,
  defaultDock,
  dockHeight,
  dockLowest,
  dockMax,
  dockMin,
  parseDockHeight,
  reserveOf,
  type DockSpace,
} from "./dock";

const space = (available: number, reserve = 230, chrome = 300): DockSpace => ({
  available,
  reserve,
  chrome,
});

describe("dock sizing", () => {
  it("reserves the measured toolbar and ruler plus a lane", () => {
    expect(reserveOf(70, 62)).toBe(232);
  });

  it("keeps the grid and the arrangement visible when clamping", () => {
    expect(dockMin(true, 300)).toBe(300 + MIN_GRID);
    expect(clampDock(10, space(900), true)).toBe(460);
    expect(clampDock(5000, space(900), true)).toBe(670);
    expect(clampDock(500, space(900), true)).toBe(500);
  });

  it("shrinks below the preferred minimum rather than overflowing when space is short", () => {
    const tight = space(600); // 370 px spare, preferred minimum 460
    expect(dockMax(tight, true)).toBe(370);
    expect(dockLowest(tight, true)).toBe(370);
    expect(clampDock(500, tight, true)).toBe(370);
    expect(clampDock(10, tight, true)).toBe(370);
  });

  it("never goes below the floor, and tolerates bad values", () => {
    const tiny = space(200);
    expect(dockMax(tiny, true)).toBe(300 + FLOOR_GRID);
    expect(clampDock(Number.NaN, space(900), false)).toBe(120);
    expect(dockMax(tiny, false)).toBe(80);
  });

  it("defaults to 45% of the space, at least 420, with a roll open", () => {
    expect(defaultDock(space(1200), true)).toBe(540);
    expect(defaultDock(space(800), true)).toBe(460);
    expect(defaultDock(space(1100), true)).toBe(495);
    expect(defaultDock(space(900), false)).toBe(DOCK_PLAIN_HEIGHT);
  });

  it("uses a saved height only for a roll, clamped to the current space", () => {
    expect(dockHeight(null, space(1200), true)).toBe(540);
    expect(dockHeight(600, space(1200), true)).toBe(600);
    expect(dockHeight(900, space(700), true)).toBe(470);
    expect(dockHeight(600, space(1200), false)).toBe(DOCK_PLAIN_HEIGHT);
  });

  it("parses stored heights", () => {
    expect(parseDockHeight(null)).toBeNull();
    expect(parseDockHeight("")).toBeNull();
    expect(parseDockHeight("abc")).toBeNull();
    expect(parseDockHeight("-4")).toBeNull();
    expect(parseDockHeight("431.6")).toBe(432);
  });
});
