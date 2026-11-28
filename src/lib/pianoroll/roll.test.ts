import { describe, expect, it } from "vitest";
import type { Clip, Controller, Note } from "../ipc/types";
import {
  CC_PRESETS,
  ccEvents,
  ccLabel,
  ccValueAtY,
  clampController,
  clearController,
  eraseNear,
  paintStroke,
} from "./cc";
import { EditQueue } from "./queue";
import {
  idsAt,
  deleteNotes,
  duplicateNotes,
  indicesOf,
  isClean,
  moveNotes,
  resizeNotes,
  scaleVelocities,
} from "./edit";
import { dragMarquee, dragPreview, marqueeIds, press } from "./gesture";
import { noteRect } from "./grid";
import { rollKeyAction } from "./keys";
import { noteIds, replaceContentCommand, sameControllers, sameNotes } from "./notes";
import { SCALES, ROOTS, rowKind, scaleById } from "./scale";
import {
  DEFAULT_OPS,
  DEFAULT_PREFS,
  humanizeEdit,
  legatoEdit,
  nextSeed,
  parsePrefs,
  quantizeEdit,
  transposeEdit,
  velocityScaleEdit,
  velocitySetEdit,
} from "./toolbar";
import { barRect, hitBar, velocityAtY } from "./velocity";

const view = { pxPerBeat: 100, pxPerKey: 10 };
const n = (start: number, length: number, key: number, velocity = 100): Note => ({ start, length, key, velocity });
const GRID = 0.25;
const CLIP = 8;
const none = { shift: false, ctrl: false };
const opts = (tool: "draw" | "select" = "select", extra = none) => ({ tool, grid: GRID, clipLength: CLIP, ...extra });

describe("group move", () => {
  const notes = [n(0, 1, 60), n(1, 1, 64), n(4, 1, 60)];

  it("moves the whole selection by the anchor's snapped travel", () => {
    const r = moveNotes(notes, [0, 1], 0, 0.6, 2, 0.5, CLIP)!;
    expect(r.notes).toEqual([n(0.5, 1, 62), n(1.5, 1, 66), n(4, 1, 60)]);
    expect(r.selected).toEqual(["62:0.5", "66:1.5"]);
  });

  it("keeps the group in the clip and the keyboard", () => {
    expect(moveNotes(notes, [0, 1], 0, -5, 0, GRID, CLIP)).toBeNull();
    const far = moveNotes(notes, [1, 2], 1, 100, 0, GRID, CLIP)!;
    expect(far.notes.at(-1)!.start).toBe(7);
    expect(moveNotes([n(0, 1, 127), n(0, 1, 120)], [0, 1], 0, 0, 5, GRID, CLIP)).toBeNull();
  });

  it("refuses a move onto an unselected note of the same key", () => {
    expect(moveNotes(notes, [0], 0, 4, 0, GRID, CLIP)).toBeNull();
    expect(moveNotes(notes, [0], 0, 4, 1, GRID, CLIP)).not.toBeNull();
  });

  it("copies leave the originals and select the copies", () => {
    const r = moveNotes(notes, [0], 0, 2, 0, GRID, CLIP, true)!;
    expect(r.notes).toHaveLength(4);
    expect(r.selected).toEqual(["60:2"]);
    expect(isClean(r.notes)).toBe(true);
    expect(moveNotes(notes, [0], 0, 0, 0, GRID, CLIP, true)).toBeNull(); // copy onto itself overlaps
  });
});

describe("group resize", () => {
  const notes = [n(0, 1, 60), n(2, 1, 60), n(2, 2, 64)];

  it("resizes the end of every selected note by the same amount", () => {
    const r = resizeNotes(notes, [0, 2], 0, "end", 1.5, GRID, CLIP)!;
    expect(r.notes.find((x) => x.key === 60 && x.start === 0)!.length).toBe(1.5);
    expect(r.notes.find((x) => x.key === 64)!.length).toBe(2.5);
  });

  it("stops at the next note and keeps one grid step", () => {
    expect(resizeNotes(notes, [0], 0, "end", 5, GRID, CLIP)!.notes[0]!.length).toBe(2);
    expect(resizeNotes(notes, [0], 0, "end", 0, GRID, CLIP)!.notes[0]!.length).toBe(GRID);
    expect(resizeNotes(notes, [0], 0, "end", 1.1, GRID, CLIP)).toBeNull();
  });

  it("resizes the start edge, keeping the end, and selects the new ids", () => {
    const r = resizeNotes(notes, [1], 1, "start", 3, GRID, CLIP)!;
    expect(r.notes.find((x) => x.key === 60 && x.start === 2.75)).toEqual(n(2.75, 0.25, 60));
  });

  it("does not run the start edge into the previous note or before zero", () => {
    const r = resizeNotes([n(0, 1, 60), n(2, 1, 60)], [1], 1, "start", 0, GRID, CLIP)!;
    expect(r.notes[1]).toEqual(n(1, 2, 60));
    expect(r.selected).toEqual(["60:1"]);
    expect(resizeNotes([n(1, 1, 60)], [0], 0, "start", -3, GRID, CLIP)!.notes[0]).toEqual(n(0, 2, 60));
  });
});

describe("duplicate, delete, velocity scaling", () => {
  it("duplicates after the group's span, cutting at the clip end", () => {
    const r = duplicateNotes([n(0, 1, 60), n(1, 1, 64)], [0, 1], GRID, CLIP)!;
    expect(r.selected).toEqual(["60:2", "64:3"]);
    expect(duplicateNotes([n(7, 1, 60)], [0], GRID, CLIP)).toBeNull();
    expect(duplicateNotes([n(6.5, 1, 60)], [0], GRID, 8)!.notes.at(-1)!.length).toBeLessThan(1);
    expect(duplicateNotes([n(0, 1, 60), n(1, 1, 60)], [0], GRID, CLIP)).toBeNull(); // spot taken
  });

  it("deletes by index", () => {
    expect(deleteNotes([n(0, 1, 60), n(1, 1, 61)], [0])).toEqual([n(1, 1, 61)]);
  });

  it("scales the group by the anchor's ratio within 1 to 127", () => {
    const notes = [n(0, 1, 60, 100), n(1, 1, 61, 50), n(2, 1, 62, 80)];
    const out = scaleVelocities(notes, [0, 1], 0, 50)!;
    expect(out.map((x) => x.velocity)).toEqual([50, 25, 80]);
    expect(scaleVelocities(notes, [0, 1], 0, 127)!.map((x) => x.velocity)).toEqual([127, 64, 80]);
    expect(scaleVelocities(notes, [0], 0, 100)).toBeNull();
    expect(scaleVelocities(notes, [1], 1, 0)![1]!.velocity).toBe(1);
  });

  it("maps selection ids to indices", () => {
    expect(indicesOf([n(0, 1, 60), n(1, 1, 62)], new Set(["62:1"]))).toEqual([1]);
  });
});

describe("gestures with selection", () => {
  const notes = [n(1, 1, 60), n(3, 1, 64)];
  const rect = (i: number) => noteRect(view, notes[i]!);

  it("select tool: a press on empty space starts a marquee, shift keeps the selection", () => {
    const a = press(notes, new Set(["60:1"]), view, 5, 5, opts());
    expect(a).toMatchObject({ type: "marquee", selection: [] });
    const b = press(notes, new Set(["60:1"]), view, 5, 5, opts("select", { shift: true, ctrl: false }));
    expect(b).toMatchObject({ type: "marquee", selection: ["60:1"] });
  });

  it("draw tool: a press on empty space draws and selects", () => {
    const r = press([], new Set(), view, rect(0).x + 30, rect(0).y + 3, opts("draw"));
    expect(r).toMatchObject({ type: "add", selection: ["60:1.25"] });
    expect(press([], new Set(), view, 99_999, 3, opts("draw"))).toEqual({ type: "none", selection: [] });
  });

  it("a press on a selected note keeps the group; releasing without moving selects just it", () => {
    const sel = new Set(["60:1", "64:3"]);
    const r = press(notes, sel, view, rect(0).x + 30, rect(0).y + 2, opts());
    expect(r).toMatchObject({ type: "drag", selection: ["60:1", "64:3"] });
    expect(r.type === "drag" && r.drag.clickSelection).toEqual(["60:1"]);
    expect(r.type === "drag" && r.drag.indices).toEqual([0, 1]);
  });

  it("shift toggles a note out of the selection and into it", () => {
    const shift = { shift: true, ctrl: false };
    const out = press(notes, new Set(["60:1"]), view, rect(0).x + 30, rect(0).y + 2, opts("select", shift));
    expect(out).toEqual({ type: "select", selection: [] });
    const into = press(notes, new Set(["60:1"]), view, rect(1).x + 30, rect(1).y + 2, opts("select", shift));
    expect(into).toMatchObject({ type: "drag", selection: ["60:1", "64:3"] });
  });

  it("dragging the group moves it, ctrl drags copies, edges resize", () => {
    const sel = new Set(["60:1", "64:3"]);
    const g = press(notes, sel, view, rect(0).x + 30, rect(0).y + 2, opts());
    if (g.type !== "drag") throw new Error("drag");
    const p = dragPreview(g.drag, view, rect(0).x + 130, rect(0).y + 2, GRID, CLIP)!;
    expect(p.notes).toEqual([n(2, 1, 60), n(4, 1, 64)]);
    const c = press(notes, sel, view, rect(0).x + 30, rect(0).y + 2, opts("select", { shift: false, ctrl: true }));
    if (c.type !== "drag") throw new Error("drag");
    expect(dragPreview(c.drag, view, rect(0).x + 130, rect(0).y + 2, GRID, CLIP)!.notes).toHaveLength(4);
    const e = press(notes, new Set(), view, rect(0).x + 100 - 2, rect(0).y + 2, opts());
    if (e.type !== "drag") throw new Error("drag");
    expect(e.drag.kind).toBe("resize-end");
    const s = press(notes, new Set(), view, rect(0).x + 2, rect(0).y + 2, opts());
    if (s.type !== "drag") throw new Error("drag");
    expect(s.drag.kind).toBe("resize-start");
    expect(dragPreview(s.drag, view, rect(0).x + 50, rect(0).y + 2, 0.5, CLIP)!.notes[0]).toEqual(n(1.5, 0.5, 60));
  });

  it("a marquee selects the notes it touches and can add to a base", () => {
    expect(marqueeIds(notes, view, { x0: 0, y0: 0, x1: 150, y1: 2000, base: [] })).toEqual(["60:1"]);
    expect(marqueeIds(notes, view, { x0: 0, y0: 0, x1: 2000, y1: 2000, base: [] })).toHaveLength(2);
    const m = dragMarquee(notes, view, { x0: 0, y0: 0, x1: 0, y1: 0, base: ["64:3"] }, 150, 2000);
    expect(m.selection.sort()).toEqual(["60:1", "64:3"]);
  });
});

describe("velocity lane", () => {
  it("maps height to velocity and back", () => {
    expect(barRect(view, n(2, 1, 60, 127), 72)).toMatchObject({ x: 200, y: 0, height: 72 });
    expect(velocityAtY(0, 72)).toBe(127);
    expect(velocityAtY(72, 72)).toBe(1);
    expect(velocityAtY(36, 72)).toBe(64);
  });

  it("picks the stacked bar whose top is nearest the pointer", () => {
    const chord = [n(0, 1, 60, 127), n(0, 1, 64, 50)];
    expect(hitBar(chord, view, 3, 2, 72)).toBe(0);
    expect(hitBar(chord, view, 3, barRect(view, chord[1]!, 72).y + 1, 72)).toBe(1);
    expect(hitBar(chord, view, 300, 10, 72)).toBeNull();
  });
});

describe("controller lane", () => {
  const cc = (beat: number, value: number, controller = 1): Controller => ({ beat, controller, value });

  it("paints one event per grid line along a stroke and replaces what is there", () => {
    const first = paintStroke([cc(0.5, 10), cc(0.5, 9, 7)], 1, null, { beat: 0.5, value: 90 }, 0.5, CLIP);
    expect(first).toEqual([cc(0.5, 9, 7), cc(0.5, 90)].sort((a, b) => a.beat - b.beat));
    const stroke = paintStroke([], 1, { beat: 0, value: 0 }, { beat: 2, value: 100 }, 0.5, CLIP);
    expect(stroke.map((e) => e.beat)).toEqual([0, 0.5, 1, 1.5, 2]);
    expect(stroke.map((e) => e.value)).toEqual([0, 25, 50, 75, 100]);
    const back = paintStroke([], 1, { beat: 2, value: 100 }, { beat: 0, value: 0 }, 0.5, CLIP);
    expect(back.map((e) => e.value)).toEqual([0, 25, 50, 75, 100]);
  });

  it("stays inside the clip and keeps other controllers", () => {
    const out = paintStroke([cc(1, 5, 7)], 1, null, { beat: CLIP + 3, value: 5 }, GRID, CLIP);
    expect(out).toEqual([cc(1, 5, 7)]);
    expect(paintStroke([], 1, null, { beat: -0.1, value: 5 }, GRID, CLIP)).toEqual([cc(0, 5)]);
  });

  it("filters, erases and clears one controller", () => {
    const all = [cc(0, 1), cc(1, 2, 7), cc(2, 3)];
    expect(ccEvents(all, 1).map((e) => e.beat)).toEqual([0, 2]);
    expect(eraseNear(all, 1, view, 203)).toEqual([cc(0, 1), cc(1, 2, 7)]);
    expect(eraseNear(all, 1, view, 150)).toBeNull();
    expect(eraseNear(all, 7, view, 0)).toBeNull();
    expect(clearController(all, 1)).toEqual([cc(1, 2, 7)]);
  });

  it("labels and clamps controller numbers", () => {
    expect(CC_PRESETS[0]!.controller).toBe(1);
    expect(ccLabel(1)).toBe("CC 1 Mod wheel");
    expect(ccLabel(20)).toBe("CC 20");
    expect(clampController(300)).toBe(127);
    expect(clampController(-4)).toBe(0);
    expect(clampController(NaN)).toBe(1);
    expect(ccValueAtY(0, 72)).toBe(127);
    expect(ccValueAtY(72, 72)).toBe(0);
  });
});

describe("scales", () => {
  it("shades the root, scale tones and the rest", () => {
    const major = scaleById("major");
    expect(rowKind(60, 0, major)).toBe("root");
    expect(rowKind(64, 0, major)).toBe("scale");
    expect(rowKind(61, 0, major)).toBe("out");
    expect(rowKind(62, 2, major)).toBe("root"); // D major
    expect(rowKind(61, 2, major)).toBe("scale");
    expect(rowKind(60, 0, scaleById("off"))).toBe("plain");
  });

  it("has the required scales and twelve roots", () => {
    const ids = SCALES.map((s) => s.id);
    for (const id of ["major", "minor", "dorian", "pentatonic-major", "pentatonic-minor"]) expect(ids).toContain(id);
    expect(ROOTS).toHaveLength(12);
    expect(scaleById("nope").id).toBe("off");
  });
});

describe("toolbar operations", () => {
  it("builds midi_edit operations from the settings", () => {
    expect(quantizeEdit(0.25, { ...DEFAULT_OPS, strength: 50, lengths: true })).toEqual({
      op: "quantize",
      grid: 0.25,
      strength: 0.5,
      lengths: true,
    });
    expect(quantizeEdit(0.25, { ...DEFAULT_OPS, strength: 900 })).toMatchObject({ strength: 1 });
    expect(legatoEdit({ ...DEFAULT_OPS, legato: "same_key" })).toEqual({ op: "legato", mode: "same_key" });
    expect(humanizeEdit({ ...DEFAULT_OPS, seed: 7.9, timing: 5, velocity: -3 })).toEqual({
      op: "humanize",
      seed: 7,
      timing: 1,
      velocity: 0,
    });
    expect(transposeEdit(-12)).toEqual({ op: "transpose", semitones: -12 });
    expect(velocitySetEdit({ ...DEFAULT_OPS, setTo: 300 })).toEqual({ op: "velocity_set", velocity: 127 });
    expect(velocityScaleEdit({ ...DEFAULT_OPS, scalePercent: 50 })).toEqual({ op: "velocity_scale", factor: 0.5 });
    expect(nextSeed(1)).toBe(2);
  });

  it("restores valid preferences and ignores bad ones", () => {
    expect(parsePrefs(null)).toEqual(DEFAULT_PREFS);
    expect(parsePrefs("{oops")).toEqual(DEFAULT_PREFS);
    expect(parsePrefs(JSON.stringify({ grid: 0.5, root: 9, scale: "minor", controller: 74, tool: "select" }))).toEqual({
      grid: 0.5,
      root: 9,
      scale: "minor",
      controller: 74,
      tool: "select",
    });
    expect(parsePrefs(JSON.stringify({ grid: 3, root: 99, scale: "x", controller: 500, tool: 4 }))).toEqual(DEFAULT_PREFS);
  });
});

describe("keys and commands", () => {
  const key = (k: string, extra: Partial<{ ctrlKey: boolean; shiftKey: boolean; altKey: boolean }> = {}) =>
    rollKeyAction({ key: k, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...extra });

  it("maps roll shortcuts", () => {
    expect(key("Delete")).toBe("delete");
    expect(key("a", { ctrlKey: true })).toBe("select-all");
    expect(key("d", { ctrlKey: true })).toBe("duplicate");
    expect(key("ArrowUp", { shiftKey: true })).toBe("octave-up");
    expect(key("ArrowLeft")).toBe("nudge-left");
    expect(key("d")).toBe("tool-draw");
    expect(key("v")).toBe("tool-select");
    expect(key("q")).toBe("quantize");
    expect(key("x")).toBeNull();
    expect(key("d", { altKey: true })).toBeNull();
  });

  it("replaces notes and/or controllers in one replace_clip, keeping the other", () => {
    const clip: Clip = {
      id: 1,
      track: 1,
      name: "c",
      start: 0,
      length: 4,
      muted: false,
      content: { type: "midi", notes: [n(0, 1, 60)], controllers: [{ beat: 1, controller: 1, value: 2 }] },
      take: null,
    };
    const onlyCc = replaceContentCommand(clip, { controllers: [] });
    expect(onlyCc).toMatchObject({ clip: { content: { notes: [n(0, 1, 60)], controllers: [] } } });
    const onlyNotes = replaceContentCommand(clip, { notes: [] });
    expect(onlyNotes).toMatchObject({ clip: { content: { notes: [], controllers: [{ beat: 1, controller: 1, value: 2 }] } } });
  });
});

describe("edit queue", () => {
  /** A sender whose first call blocks until released. */
  function blocked<K, T>() {
    const sent: [K, T][] = [];
    let release!: () => void;
    const send = async (key: K, state: T) => {
      sent.push([key, state]);
      if (sent.length === 1) await new Promise<void>((resolve) => (release = resolve));
    };
    return { sent, send, release: () => release() };
  }

  it("sends the first edit at once and folds rapid ones into a single follow-up", async () => {
    const b = blocked<number, Note[]>();
    const queue = new EditQueue<number, Note[]>(b.send);
    // Three nudges of one grid step, each built from the latest state shown.
    let shown: Note[] = [n(0, 1, 60)];
    for (let i = 0; i < 3; i++) {
      shown = moveNotes(shown, [0], 0, GRID, 0, GRID, CLIP)!.notes;
      queue.push(1, shown);
    }
    expect(b.sent).toHaveLength(1);
    expect(queue.busy).toBe(true);
    b.release();
    await queue.idle();
    expect(b.sent).toHaveLength(2);
    expect(b.sent[1]![1][0]!.start).toBe(0.75);
    expect(queue.busy).toBe(false);
  });

  it("sends each state to its own clip when the target switches", async () => {
    const b = blocked<number, string>();
    const queue = new EditQueue<number, string>(b.send);
    queue.push(1, "a1");
    queue.push(1, "a2"); // folded into one follow-up for clip 1
    queue.push(2, "b1"); // another clip: never replaces clip 1's state
    b.release();
    await queue.idle();
    expect(b.sent).toEqual([
      [1, "a1"],
      [1, "a2"],
      [2, "b1"],
    ]);
  });

  it("drops the waiting follow-up of a refused send and reports it, others still go out", async () => {
    const failed: number[] = [];
    const sent: [number, string][] = [];
    let release!: () => void;
    const queue = new EditQueue<number, string>(
      async (key, state) => {
        sent.push([key, state]);
        if (sent.length === 1) {
          await new Promise<void>((resolve) => (release = resolve));
          return false;
        }
      },
      { onFail: (key) => failed.push(key) },
    );
    queue.push(1, "a1");
    queue.push(1, "a2"); // built on a1, which is refused
    queue.push(2, "b1");
    release();
    await queue.idle();
    expect(sent).toEqual([
      [1, "a1"],
      [2, "b1"],
    ]);
    expect(failed).toEqual([1]);
  });

  it("treats a thrown send like a refusal and keeps going", async () => {
    const failed: number[] = [];
    let idle = 0;
    const queue = new EditQueue<number, number>(
      async () => {
        throw new Error("refused");
      },
      { onIdle: () => idle++, onFail: (k) => failed.push(k) },
    );
    queue.push(1, 1);
    await queue.idle();
    expect(failed).toEqual([1]);
    expect(idle).toBe(1);
    expect(queue.busy).toBe(false);
  });

  it("resolves every idle waiter once, and a push during onIdle is sent before they resolve", async () => {
    const sent: number[] = [];
    let pushed = false;
    const queue: EditQueue<number, number> = new EditQueue<number, number>(
      async (_, state) => {
        sent.push(state);
      },
      {
        onIdle: () => {
          if (!pushed) {
            pushed = true;
            queue.push(1, 2);
          }
        },
      },
    );
    queue.push(1, 1);
    const order: string[] = [];
    const a = queue.idle().then(() => order.push(`a${sent.length}`));
    const b = queue.idle().then(() => order.push(`b${sent.length}`));
    await Promise.all([a, b]);
    expect(sent).toEqual([1, 2]);
    expect(order).toEqual(["a2", "b2"]);
    await expect(queue.idle()).resolves.toBeUndefined();
  });
});

describe("notes that share key and start", () => {
  it("builds the selection of some indices", () => {
    expect(idsAt([n(0, 1, 60), n(1, 1, 61), n(2, 1, 62)], [0, 2])).toEqual(["60:0", "62:2"]);
  });

  // Quantize can put two notes of one key on one start.
  const twins = [n(1, 0.5, 60), n(1, 0.5, 60, 80), n(2, 1, 62)];

  it("gives every note its own id, in list order", () => {
    expect(noteIds(twins)).toEqual(["60:1", "60:1#1", "62:2"]);
    expect(new Set(noteIds(twins)).size).toBe(3);
  });

  it("selects exactly one of the twins and edits only that one", () => {
    expect(indicesOf(twins, new Set(["60:1#1"]))).toEqual([1]);
    expect(indicesOf(twins, new Set(["60:1"]))).toEqual([0]);
    const r = moveNotes(twins, [1], 1, 0, 1, GRID, CLIP)!;
    expect(r.notes).toHaveLength(3);
    expect(r.notes.filter((x) => x.key === 60)).toHaveLength(1);
    expect(r.selected).toEqual(["61:1"]);
    const rect = noteRect(view, twins[1]!);
    // The topmost (last) twin is the one grabbed.
    expect(press(twins, new Set(), view, rect.x + 30, rect.y + 2, opts())).toMatchObject({
      type: "drag",
      selection: ["60:1#1"],
    });
  });

  it("marquee names every twin", () => {
    expect(marqueeIds(twins, view, { x0: 0, y0: 0, x1: 5000, y1: 5000, base: [] })).toEqual(noteIds(twins));
  });

  it("compares note and controller lists by content", () => {
    expect(sameNotes([n(0, 1, 60)], [n(0, 1, 60)])).toBe(true);
    expect(sameNotes([n(0, 1, 60)], [n(0, 1, 60, 99)])).toBe(false);
    expect(sameControllers([{ beat: 0, controller: 1, value: 3 }], [{ beat: 0, controller: 1, value: 3 }])).toBe(true);
    expect(sameControllers([], [{ beat: 0, controller: 1, value: 3 }])).toBe(false);
  });
});
