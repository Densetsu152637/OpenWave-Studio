import { describe, expect, it } from "vitest";
import type { Clip, EditCommand, Marker, Track } from "../ipc/types";
import { moveBus, setBusColor } from "../mixer/mixer";
import {
  deleteClips,
  moveClips,
  moveTrack,
  reorderTarget,
  setTrackColor,
  type ArrangeApi,
} from "./edits";
import { arrangeKeyAction, ARRANGE_SHORTCUTS } from "./keys";
import {
  addMarker,
  nextMarkerName,
  removeMarker,
  renameMarker,
  renameMarkerCommand,
} from "./markers";
import { formatTime, timeMarks } from "./ruler";
import {
  NO_SELECTION,
  pressClip,
  pruneSelection,
  releaseClip,
  selectAll,
} from "./selection";
import { dragDelta, snapBeat } from "./snap";
import { SignatureMap, TempoMap } from "./time";
import {
  MAX_PX_PER_BEAT,
  MIN_PX_PER_BEAT,
  dragClipStart,
  projectEndBeat,
  visibleClips,
  visibleRows,
  zoomAt,
} from "./view";
import { SHORTCUT_ACTIONS } from "../prefs";
import { percentile, pingPong, runScrollBenchmark, summarizeFrames } from "../perf/scroll";

const common = new SignatureMap([{ bar: 0, numerator: 4, denominator: 4 }]);
const view = { originBeat: 0, pxPerBeat: 10 };

function recorder() {
  const calls: string[] = [];
  const edits: EditCommand[] = [];
  const api: ArrangeApi = {
    edit: async (edit) => {
      edits.push(edit);
      calls.push(edit.command);
    },
    begin: async (label) => void calls.push(`begin:${label}`),
    end: async () => void calls.push("end"),
    cancel: async () => void calls.push("cancel"),
    allocIds: async (count) => Array.from({ length: count }, (_, i) => 50 + i),
    mediaImport: async () => ({ media: 1, frames: 1, sampleRate: 48_000, channels: 1 }),
  };
  return { api, calls, edits };
}

const clipAt = (id: number, start: number): Clip => ({
  id,
  track: 1,
  name: `c${id}`,
  start,
  length: 4,
  muted: false,
  content: { type: "midi", notes: [], controllers: [] },
});

describe("zoom", () => {
  it("keeps the beat under the anchor and clamps to the limits", () => {
    const zoomed = zoomAt({ originBeat: 8, pxPerBeat: 20 }, 2, 100);
    expect(zoomed.pxPerBeat).toBe(40);
    // The beat at x = 100 was 13 and still is.
    expect(zoomed.originBeat + 100 / zoomed.pxPerBeat).toBeCloseTo(13, 12);
    expect(zoomAt(view, 1e6, 0).pxPerBeat).toBe(MAX_PX_PER_BEAT);
    expect(zoomAt(view, 1e-6, 0).pxPerBeat).toBe(MIN_PX_PER_BEAT);
  });

  it("never scrolls before the start of the timeline", () => {
    expect(zoomAt({ originBeat: 0, pxPerBeat: 20 }, 0.5, 300).originBeat).toBe(0);
  });
});

describe("snap grid", () => {
  it("snaps to the nearest bar, the default", () => {
    expect(snapBeat(5.9, "bar", common)).toBe(4);
    expect(snapBeat(6.1, "bar", common)).toBe(8);
  });

  it("snaps to each beat fraction", () => {
    expect(snapBeat(1.4, "1", common)).toBe(1);
    expect(snapBeat(1.3, "1/2", common)).toBe(1.5);
    expect(snapBeat(1.3, "1/4", common)).toBe(1.25);
    expect(snapBeat(1.3, "1/8", common)).toBe(1.25);
    expect(snapBeat(1.3, "1/16", common)).toBeCloseTo(1.3125, 12);
  });

  it("leaves the position alone when off or when Alt bypasses the grid", () => {
    expect(snapBeat(1.234, "off", common)).toBe(1.234);
    expect(snapBeat(1.234, "bar", common, true)).toBe(1.234);
    expect(snapBeat(1.234, "1/16", common, true)).toBe(1.234);
  });

  it("never goes before beat 0", () => {
    expect(snapBeat(-3, "1/4", common)).toBe(0);
    expect(snapBeat(-3, "off", common)).toBe(0);
  });

  it("applies to a dragged clip start, with the bar as the default", () => {
    expect(dragClipStart(4, 30, view, common)).toBe(8);
    expect(dragClipStart(4, 12.5, view, common, "1/4")).toBe(5.25);
    expect(dragClipStart(4, 12.5, view, common, "bar", true)).toBe(5.25);
  });

  it("shifts a selection by one snapped delta without crossing beat 0", () => {
    // The dragged clip starts at 4 and moves 30 px = 3 beats: it lands on bar 8, a shift of 4.
    expect(dragDelta(4, 4, 30, view, common, "bar")).toBe(4);
    expect(dragDelta(4, 4, 30, view, common, "off", true)).toBe(3);
    // Another selected clip at beat 2 limits a leftward drag.
    expect(dragDelta(4, 2, -500, view, common, "bar")).toBe(-2);
  });
});

describe("selection model", () => {
  it("selects one clip, and Shift/Ctrl-click toggles more in and out", () => {
    let sel = pressClip(NO_SELECTION, 1, false);
    expect([...sel]).toEqual([1]);
    sel = pressClip(sel, 2, true);
    expect([...sel].sort()).toEqual([1, 2]);
    sel = pressClip(sel, 1, true);
    expect([...sel]).toEqual([2]);
    expect([...pressClip(sel, 3, false)]).toEqual([3]);
  });

  it("keeps a multi-selection when pressing inside it, narrowing it on a plain click", () => {
    const sel = selectAll([1, 2, 3]);
    expect(pressClip(sel, 2, false)).toBe(sel);
    expect([...releaseClip(sel, 2, false, false)]).toEqual([2]);
    expect(releaseClip(sel, 2, false, true)).toBe(sel);
    expect(releaseClip(sel, 2, true, false)).toBe(sel);
  });

  it("selects all and drops clips that are gone", () => {
    const sel = selectAll([1, 2, 3]);
    expect(sel.size).toBe(3);
    const kept = pruneSelection(sel, (id) => id !== 2);
    expect([...kept]).toEqual([1, 3]);
    expect(pruneSelection(kept, () => true)).toBe(kept);
  });
});

describe("clip edits", () => {
  it("moves every selected clip by the same delta in one transaction", async () => {
    const { api, calls, edits } = recorder();
    const moved = await moveClips(api, [clipAt(1, 4), clipAt(2, 8), clipAt(3, 20)], 4);
    expect(moved).toBe(true);
    expect(calls).toEqual(["begin:Move clips", "replace_clip", "replace_clip", "replace_clip", "end"]);
    expect(edits.map((e) => (e.command === "replace_clip" ? e.clip.start : null))).toEqual([8, 12, 24]);
  });

  it("clamps at beat 0 and sends nothing when nothing moves", async () => {
    const { api, edits, calls } = recorder();
    await moveClips(api, [clipAt(1, 1)], -4);
    expect(edits.map((e) => (e.command === "replace_clip" ? e.clip.start : null))).toEqual([0]);
    const none = recorder();
    expect(await moveClips(none.api, [clipAt(1, 0)], -4)).toBe(false);
    expect(none.calls).toEqual([]);
    expect(calls[0]).toBe("begin:Move clip");
  });

  it("deletes all selected clips in one transaction", async () => {
    const { api, calls } = recorder();
    await deleteClips(api, [clipAt(1, 0), clipAt(2, 4)]);
    expect(calls).toEqual(["begin:Delete clips", "remove_clip", "remove_clip", "end"]);
  });

  it("finds the end of the project", () => {
    expect(projectEndBeat([clipAt(1, 4), clipAt(2, 20)])).toBe(24);
    expect(projectEndBeat([])).toBe(0);
  });
});

describe("track and bus edits", () => {
  const track = { id: 2, name: "Keys", color: 0x111111 } as Track;

  it("recolors a track with SetInfo, keeping its name", async () => {
    const { api, edits } = recorder();
    expect(await setTrackColor(api, track, 0x222222)).toBe(true);
    expect(edits).toEqual([
      { command: "set_info", target: { track: 2 }, name: "Keys", color: 0x222222 },
    ]);
    expect(await setTrackColor(api, track, 0x111111)).toBe(false);
    expect(edits.length).toBe(1);
  });

  it("works out where an up or down move lands", () => {
    expect(reorderTarget([5, 6, 7], 6, -1)).toBe(0);
    expect(reorderTarget([5, 6, 7], 6, 1)).toBe(2);
    expect(reorderTarget([5, 6, 7], 5, -1)).toBeNull();
    expect(reorderTarget([5, 6, 7], 7, 1)).toBeNull();
    expect(reorderTarget([5, 6, 7], 9, 1)).toBeNull();
  });

  it("reorders with one MoveTrack or MoveBus, recoloring a bus with SetInfo", async () => {
    const { api, edits } = recorder();
    await moveTrack(api, 6, 0);
    await moveBus(api, 9, 1);
    await setBusColor(api, { id: 9, name: "Drums", color: 1 } as never, 2);
    expect(edits).toEqual([
      { command: "move_track", id: 6, to: 0 },
      { command: "move_bus", id: 9, to: 1 },
      { command: "set_info", target: { bus: 9 }, name: "Drums", color: 2 },
    ]);
  });
});

describe("markers", () => {
  const marker: Marker = { id: 7, name: "Verse", beat: 16, color: 0xf2cf4b };

  it("adds a marker with a reserved id and the next free name", async () => {
    const { api, edits } = recorder();
    const id = await addMarker(api, 8, [{ name: "Marker 1" }]);
    expect(id).toBe(50);
    expect(edits).toEqual([
      { command: "add_marker", marker: { id: 50, name: "Marker 2", beat: 8, color: 0xf2cf4b } },
    ]);
    expect(nextMarkerName([])).toBe("Marker 1");
  });

  it("renames by replacing the marker with the same id and position", async () => {
    const { api, edits } = recorder();
    expect(await renameMarker(api, marker, "  Chorus ")).toBe(true);
    expect(edits).toEqual([renameMarkerCommand(marker, "Chorus")]);
    expect(edits[0]).toEqual({ command: "replace_marker", marker: { ...marker, name: "Chorus" } });
    expect(await renameMarker(api, marker, "Verse")).toBe(false);
    expect(await renameMarker(api, marker, "   ")).toBe(false);
    expect(edits.length).toBe(1);
  });

  it("deletes with RemoveMarker", async () => {
    const { api, edits } = recorder();
    await removeMarker(api, 7);
    expect(edits).toEqual([{ command: "remove_marker", id: 7 }]);
  });
});

describe("time ruler", () => {
  it("formats mm:ss.mmm", () => {
    expect(formatTime(0)).toBe("00:00.000");
    expect(formatTime(1.5)).toBe("00:01.500");
    expect(formatTime(61.0004)).toBe("01:01.000");
    expect(formatTime(59.9996)).toBe("01:00.000");
    expect(formatTime(3725.25)).toBe("62:05.250");
    expect(formatTime(-2)).toBe("00:00.000");
  });

  it("marks seconds from the tempo map", () => {
    const tempo = new TempoMap([{ beat: 0, bpm: 120, ramp: false }]);
    // 20 px per beat = 40 px per second: labels every 2 s (80 px >= 72), ticks every 0.4 s.
    const marks = timeMarks({ originBeat: 0, pxPerBeat: 20 }, 400, tempo);
    const labelled = marks.filter((m) => m.label !== null);
    expect(labelled.slice(0, 3).map((m) => m.label)).toEqual(["00:00.000", "00:02.000", "00:04.000"]);
    expect(labelled[1]!.x).toBeCloseTo(80, 6);
    expect(labelled[1]!.beat).toBeCloseTo(4, 9);
    expect(marks.some((m) => !m.major)).toBe(true);
  });

  it("follows a tempo change", () => {
    const tempo = new TempoMap([
      { beat: 0, bpm: 60, ramp: false },
      { beat: 4, bpm: 120, ramp: false },
    ]);
    const marks = timeMarks({ originBeat: 0, pxPerBeat: 50 }, 600, tempo);
    const at6s = marks.find((m) => m.label === "00:06.000");
    // 4 beats take 4 s at 60 bpm; 2 s more at 120 bpm is 4 beats: beat 8.
    expect(at6s?.beat).toBeCloseTo(8, 9);
  });
});

describe("keyboard shortcuts", () => {
  const key = (k: string, extra: Partial<KeyboardEvent> = {}) =>
    arrangeKeyAction({ key: k, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...extra });

  it("maps the arrangement keys", () => {
    expect(key("Home")).toBe("locate-start");
    expect(key("End")).toBe("locate-end");
    expect(key("+")).toBe("zoom-in");
    expect(key("=")).toBe("zoom-in");
    expect(key("-")).toBe("zoom-out");
    expect(key("m")).toBe("add-marker");
    expect(key("M", { shiftKey: true })).toBe("add-marker");
    expect(key("Escape")).toBe("deselect");
    expect(key("Delete")).toBe("delete-selection");
    expect(key("Backspace")).toBe("delete-selection");
    expect(key("a", { ctrlKey: true })).toBe("select-all");
    expect(key("a", { metaKey: true })).toBe("select-all");
  });

  it("leaves the app's own and the browser's shortcuts alone", () => {
    expect(key(" ")).toBeNull();
    expect(key("z", { ctrlKey: true })).toBeNull();
    expect(key("+", { ctrlKey: true })).toBeNull();
    expect(key("m", { ctrlKey: true })).toBeNull();
    expect(key("Home", { altKey: true })).toBeNull();
    expect(key("a", { ctrlKey: true, shiftKey: true })).toBeNull();
  });

  it("lists every action once for the settings panel", () => {
    expect(new Set(ARRANGE_SHORTCUTS.map((s) => s.action)).size).toBe(11);
    expect(new Set(ARRANGE_SHORTCUTS.map((s) => s.id)).size).toBe(11);
  });
});

describe("scroll benchmark", () => {
  it("computes nearest-rank percentiles and frame statistics", () => {
    expect(percentile([], 0.95)).toBe(0);
    expect(percentile([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 0.95)).toBe(10);
    expect(percentile([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 0.5)).toBe(5);
    expect(summarizeFrames([0, 10, 20, 50])).toEqual({ frames: 3, p95Ms: 30, meanMs: 50 / 3 });
  });

  it("steps once per frame until the duration passes", async () => {
    let now = 0;
    const steps: number[] = [];
    const clock = {
      requestFrame(cb: (t: number) => void) {
        now += 16;
        queueMicrotask(() => cb(now));
      },
    };
    const result = await runScrollBenchmark(clock, (e) => steps.push(e), 160);
    expect(steps.length).toBe(10);
    expect(result.frames).toBe(10);
    expect(result.p95Ms).toBe(16);
    expect(result.meanMs).toBe(16);
  });

  it("sweeps back and forth", () => {
    expect(pingPong(0, 100, 1000)).toBe(0);
    expect(pingPong(500, 100, 1000)).toBe(100);
    expect(pingPong(1000, 100, 1000)).toBe(0);
  });
});

describe("virtualization", () => {
  it("renders only the rows in the viewport plus the overscan", () => {
    expect(visibleRows(0, 600, 88, 100, 0)).toEqual({ first: 0, last: 7 });
    expect(visibleRows(0, 600, 88, 100)).toEqual({ first: 0, last: 10 });
    expect(visibleRows(8800 - 300, 600, 88, 100, 2)).toEqual({ first: 94, last: 100 });
    expect(visibleRows(880, 600, 88, 100, 1)).toEqual({ first: 9, last: 18 });
  });

  it("copes with short lists, negative offsets and empty lists", () => {
    expect(visibleRows(-50, 600, 88, 3)).toEqual({ first: 0, last: 3 });
    expect(visibleRows(0, 600, 88, 0)).toEqual({ first: 0, last: 0 });
    expect(visibleRows(1e6, 600, 88, 5)).toEqual({ first: 5, last: 5 });
  });

  it("keeps clips that overlap the visible beats, and the ones asked for", () => {
    const clips = [clipAt(1, 0), clipAt(2, 50), clipAt(3, 100), clipAt(4, 200)];
    // 10 px per beat, 300 px wide, from beat 48: beats 48 to 78.
    const shown = visibleClips(clips, { originBeat: 48, pxPerBeat: 10 }, 300);
    expect(shown.map((c) => c.id)).toEqual([2]);
    // Clip 1 ends at beat 4; clip 3 starts at 100.
    expect(visibleClips(clips, { originBeat: 48, pxPerBeat: 10 }, 300, new Set([4])).map((c) => c.id)).toEqual([2, 4]);
    expect(visibleClips(clips, { originBeat: 0, pxPerBeat: 10 }, 5000)).toBe(clips);
  });

  it("drops a clip that ends exactly at the left edge", () => {
    expect(visibleClips([clipAt(1, 0)], { originBeat: 4, pxPerBeat: 10 }, 300)).toEqual([]);
  });
});

describe("piano roll and fixed shortcuts", () => {
  const press = (key: string) =>
    arrangeKeyAction(
      { key, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false },
      { rollOpen: true },
    );

  it("never deletes clips while the piano roll is open", () => {
    expect(press("Delete")).toBeNull();
    expect(press("Backspace")).toBeNull();
    expect(press("Escape")).toBe("deselect");
  });

  it("lists the arrangement shortcuts as fixed in settings", () => {
    const listed = SHORTCUT_ACTIONS.filter((a) => a.id.startsWith("arrange."));
    expect(listed.length).toBe(11);
    expect(listed.every((a) => a.fixed)).toBe(true);
  });
});

describe("virtualization boundaries", () => {
  it("handles an empty list and a scroll position above the first row", () => {
    expect(visibleRows(0, 600, 88, 0)).toEqual({ first: 0, last: 0 });
    expect(visibleRows(-200, 100, 88, 50, 0)).toEqual({ first: 0, last: 0 });
    expect(visibleRows(-200, 400, 88, 50, 0)).toEqual({ first: 0, last: 3 });
  });

  it("includes a row that is just touched by the viewport edge only through overscan", () => {
    expect(visibleRows(88, 88, 88, 10, 0)).toEqual({ first: 1, last: 2 });
    expect(visibleRows(100, 88, 88, 10, 0)).toEqual({ first: 1, last: 3 });
  });

  it("treats clips that only touch the view edges as hidden", () => {
    const v = { originBeat: 10, pxPerBeat: 10 };
    const edge = [clipAt(1, 6), clipAt(2, 30), clipAt(3, 29.9), clipAt(4, 6.1)];
    // Visible beats: 10 to 30; clip 1 ends at 10, clip 2 starts at 30.
    expect(visibleClips(edge, v, 200).map((c) => c.id)).toEqual([3, 4]);
    expect(visibleClips([], v, 200)).toEqual([]);
  });
});
