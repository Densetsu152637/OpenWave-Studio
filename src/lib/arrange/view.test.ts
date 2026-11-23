import { describe, expect, it } from "vitest";
import { positionLabel } from "./playback";
import { SignatureMap, TempoMap } from "./time";
import {
  MAX_PX_PER_BEAT,
  MIN_PX_PER_BEAT,
  beatToX,
  clipRect,
  clipVisibleSpan,
  dragClipStart,
  rulerMarks,
  scrollBy,
  xToBeat,
  zoomAt,
} from "./view";

const common = new SignatureMap([{ bar: 0, numerator: 4, denominator: 4 }]);

describe("tempo map", () => {
  it("converts beats and seconds at a constant tempo", () => {
    const tempo = new TempoMap([{ beat: 0, bpm: 120, ramp: false }]);
    expect(tempo.beatsToSeconds(4)).toBeCloseTo(2, 12);
    expect(tempo.secondsToBeats(2)).toBeCloseTo(4, 12);
    expect(tempo.samplesToBeats(48_000, 48_000)).toBeCloseTo(2, 12);
  });

  it("follows tempo changes and ramps, and inverts them", () => {
    const steps = new TempoMap([
      { beat: 0, bpm: 120, ramp: false },
      { beat: 4, bpm: 60, ramp: false },
    ]);
    expect(steps.beatsToSeconds(6)).toBeCloseTo(2 + 2, 12);
    expect(steps.secondsToBeats(4)).toBeCloseTo(6, 12);
    const ramp = new TempoMap([
      { beat: 0, bpm: 60, ramp: true },
      { beat: 8, bpm: 120, ramp: false },
    ]);
    // 60 L / (b1 - b0) * ln(b1 / b0) with L = 8.
    expect(ramp.beatsToSeconds(8)).toBeCloseTo((60 * 8 * Math.LN2) / 60, 10);
    for (const beat of [0, 1.5, 7.9, 8, 20]) {
      expect(ramp.secondsToBeats(ramp.beatsToSeconds(beat))).toBeCloseTo(beat, 9);
    }
  });
});

describe("signature map", () => {
  it("places bars across a signature change", () => {
    const map = new SignatureMap([
      { bar: 0, numerator: 4, denominator: 4 },
      { bar: 2, numerator: 3, denominator: 4 },
    ]);
    expect(map.barStart(2)).toBe(8);
    expect(map.barStart(4)).toBe(14);
    expect(map.barAt(9)).toMatchObject({ index: 2, start: 8, length: 3 });
    expect(map.barAt(14)).toMatchObject({ index: 4, start: 14 });
  });

  it("snaps to the nearest bar start", () => {
    expect(common.nearestBarStart(1.9)).toBe(0);
    expect(common.nearestBarStart(2.1)).toBe(4);
    expect(common.nearestBarStart(2)).toBe(0);
    expect(common.nearestBarStart(-3)).toBe(0);
    expect(common.nearestBarStart(7.99)).toBe(8);
  });
});

describe("time and pixel mapping", () => {
  const view = { originBeat: 2, pxPerBeat: 20 };

  it("maps beats to pixels and back", () => {
    expect(beatToX(view, 2)).toBe(0);
    expect(beatToX(view, 4.5)).toBe(50);
    expect(xToBeat(view, 50)).toBe(4.5);
    expect(xToBeat(view, beatToX(view, 13.25))).toBeCloseTo(13.25, 12);
  });

  it("zooms around the pixel under the pointer", () => {
    const zoomed = zoomAt(view, 2, 100);
    expect(zoomed.pxPerBeat).toBe(40);
    expect(xToBeat(zoomed, 100)).toBeCloseTo(xToBeat(view, 100), 12);
  });

  it("limits the zoom and keeps the origin at or after beat 0", () => {
    expect(zoomAt(view, 1e6, 0).pxPerBeat).toBe(MAX_PX_PER_BEAT);
    expect(zoomAt(view, 1e-6, 0).pxPerBeat).toBe(MIN_PX_PER_BEAT);
    expect(zoomAt({ originBeat: 0, pxPerBeat: 20 }, 0.5, 400).originBeat).toBe(0);
    expect(scrollBy(view, -1000).originBeat).toBe(0);
    expect(scrollBy(view, 40).originBeat).toBe(4);
  });

  it("finds the visible part of a clip", () => {
    const clip = { start: 4, length: 8 };
    expect(clipRect(view, clip)).toEqual({ x: 40, width: 160 });
    expect(clipVisibleSpan(view, clip, 100)).toEqual({ left: 0, right: 60 });
    expect(clipVisibleSpan({ ...view, originBeat: 6 }, clip, 100)).toEqual({ left: 40, right: 140 });
    expect(clipVisibleSpan(view, clip, 40)).toBeNull();
    expect(clipVisibleSpan({ ...view, originBeat: 12 }, clip, 100)).toBeNull();
  });
});

describe("clip drag", () => {
  const view = { originBeat: 0, pxPerBeat: 10 };

  it("snaps the dragged start to a bar", () => {
    expect(dragClipStart(4, 30, view, common)).toBe(8);
    expect(dragClipStart(4, 12, view, common)).toBe(4);
    expect(dragClipStart(4, 0, view, common)).toBe(4);
  });

  it("never moves before the start of the timeline", () => {
    expect(dragClipStart(4, -500, view, common)).toBe(0);
  });
});

describe("ruler", () => {
  it("marks bars and beats when zoomed in", () => {
    const marks = rulerMarks({ originBeat: 0, pxPerBeat: 20 }, 170, common);
    const bars = marks.filter((m) => m.kind === "bar");
    expect(bars.map((m) => m.label)).toEqual(["1", "2", "3"]);
    expect(bars.map((m) => m.x)).toEqual([0, 80, 160]);
    expect(marks.filter((m) => m.kind === "beat").length).toBe(9);
  });

  it("labels every few bars when zoomed out", () => {
    const marks = rulerMarks({ originBeat: 0, pxPerBeat: 2 }, 400, common);
    const labels = marks.filter((m) => m.label !== null).map((m) => m.label);
    // 8 px per bar: a label needs 48, so every 8th bar.
    expect(labels.slice(0, 3)).toEqual(["1", "9", "17"]);
    expect(marks.every((m) => m.kind === "bar")).toBe(true);
  });

  it("starts at the first bar line inside the view", () => {
    const marks = rulerMarks({ originBeat: 5, pxPerBeat: 20 }, 100, common);
    expect(marks.find((m) => m.kind === "bar")).toMatchObject({ label: "3", beat: 8 });
  });

  it("reads positions as bar.beat", () => {
    expect(positionLabel(0, common)).toBe("1.1");
    expect(positionLabel(5.5, common)).toBe("2.2");
    expect(positionLabel(7.99, common)).toBe("2.4");
  });
});
