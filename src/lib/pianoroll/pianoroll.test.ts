import { describe, expect, it } from "vitest";
import { SignatureMap } from "../arrange/time";
import type { Clip, Note } from "../ipc/types";
import {
  DEFAULT_ROLL_VIEW,
  GRID_OPTIONS,
  KEYS,
  gridLines,
  hitTest,
  initialScrollTop,
  isBlackKey,
  keyName,
  noteRect,
  pointToCell,
  rollSize,
  snapDown,
  snapNearest,
  zoomRoll,
} from "./grid";
import {
  FIXED_VELOCITY,
  addNote,
  clipNotes,
  moveNote,
  noteId,
  removeNote,
  replaceNotesCommand,
  resizeNote,
  sortNotes,
} from "./notes";

const view = { pxPerBeat: 100, pxPerKey: 10 };
const note = (start: number, length: number, key: number): Note => ({
  start,
  length,
  key,
  velocity: FIXED_VELOCITY,
});
const GRID = 0.25;
const CLIP = 4;

describe("geometry", () => {
  it("places notes by beat and key, key 127 at the top", () => {
    expect(noteRect(view, note(1.5, 0.5, 127))).toEqual({ x: 150, y: 0, width: 50, height: 10 });
    expect(noteRect(view, note(0, 1, 0))).toEqual({ x: 0, y: 1270, width: 100, height: 10 });
    expect(rollSize(view, 4)).toEqual({ width: 400, height: 1280 });
  });

  it("turns a pixel into a beat and a key, inverting noteRect", () => {
    for (const key of [0, 60, 127]) {
      const rect = noteRect(view, note(2, 1, key));
      expect(pointToCell(view, rect.x + 1, rect.y + 1)).toEqual({ beat: 2.01, key });
      expect(pointToCell(view, rect.x, rect.y + rect.height - 0.01).key).toBe(key);
    }
    // Outside the roll the key stays in range.
    expect(pointToCell(view, 0, -50).key).toBe(127);
    expect(pointToCell(view, 0, 99_999).key).toBe(0);
  });

  it("snaps down or to the nearest line, free of float noise", () => {
    expect(snapDown(1.49, 0.5)).toBe(1);
    expect(snapDown(1.5, 0.5)).toBe(1.5);
    expect(snapDown(0.3, 0.1) * 10).toBeCloseTo(3);
    expect(snapNearest(1.26, 0.5)).toBe(1.5);
    expect(snapNearest(1.25, 0.5)).toBe(1.5);
    expect(snapNearest(1.24, 0.5)).toBe(1);
    expect(GRID_OPTIONS.map((g) => g.beats)).toEqual([0.125, 0.25, 0.5, 1, 2, 4]);
  });

  it("names keys and marks the black ones", () => {
    expect(keyName(60)).toBe("C4");
    expect(keyName(69)).toBe("A4");
    expect(keyName(0)).toBe("C-1");
    expect(keyName(127)).toBe("G9");
    expect([60, 61, 62, 63, 64, 65, 66].map(isBlackKey)).toEqual([false, true, false, true, false, false, true]);
  });

  it("hits the right edge as a resize and the rest as a move; the last note wins", () => {
    const notes = [note(1, 1, 60), note(1.5, 1, 60 + 1)];
    expect(hitTest(notes, view, 150, noteRect(view, notes[0]!).y + 2)).toEqual({ index: 0, part: "body" });
    expect(hitTest(notes, view, 197, noteRect(view, notes[0]!).y + 2)).toEqual({ index: 0, part: "end" });
    expect(hitTest(notes, view, 102, noteRect(view, notes[0]!).y + 2)).toEqual({ index: 0, part: "start" });
    expect(hitTest(notes, view, 50, noteRect(view, notes[0]!).y + 2)).toBeNull();
    // Another key's row does not hit.
    expect(hitTest(notes, view, 150, noteRect(view, notes[0]!).y + 25)).toBeNull();
    // Narrow notes still have a body: the edge is at most a third.
    const narrow = [note(0, 0.03, 60)];
    expect(hitTest(narrow, view, 1.5, noteRect(view, narrow[0]!).y + 1)?.part).toBe("body");
    expect(hitTest(narrow, view, 2.5, noteRect(view, narrow[0]!).y + 1)?.part).toBe("end");
  });

  it("marks bar, beat and grid lines from the time signature, relative to the clip's place", () => {
    const signatures = new SignatureMap([]);
    const lines = gridLines(view, 0, 2, 0.5, signatures);
    expect(lines.map((l) => l.kind)).toEqual(["bar", "grid", "beat", "grid", "beat"]);
    expect(lines.map((l) => l.x)).toEqual([0, 50, 100, 150, 200]);
    // A clip that starts on beat 3 of the bar: its first line is a beat, then the bar line.
    const offset = gridLines(view, 3, 2, 1, signatures);
    expect(offset.map((l) => l.kind)).toEqual(["beat", "bar", "beat"]);
    // Very long clips thin the grid instead of producing millions of lines.
    expect(gridLines(view, 0, 1e7, 0.25, signatures).length).toBeLessThanOrEqual(4002);
  });

  it("scrolls to the notes' key range and zooms within limits", () => {
    const top = initialScrollTop([{ key: 60 }, { key: 72 }], DEFAULT_ROLL_VIEW, 200);
    const middleY = (KEYS - 1 - 66) * DEFAULT_ROLL_VIEW.pxPerKey + DEFAULT_ROLL_VIEW.pxPerKey / 2;
    expect(top).toBeCloseTo(middleY - 100);
    expect(initialScrollTop([], DEFAULT_ROLL_VIEW, 100)).toBeGreaterThan(0);
    expect(initialScrollTop([{ key: 127 }], DEFAULT_ROLL_VIEW, 100)).toBe(0);
    expect(zoomRoll(view, 1000).pxPerBeat).toBe(400);
    expect(zoomRoll(view, 0.0001).pxPerBeat).toBe(8);
  });
});

describe("drawing notes", () => {
  it("draws a one-step note on the grid line at or before the click, at velocity 100", () => {
    const notes = addNote([], CLIP, 1.3, 60, GRID)!;
    expect(notes).toEqual([{ start: 1.25, length: 0.25, key: 60, velocity: 100 }]);
  });

  it("refuses a spot that is taken, outside the clip, or off the keyboard", () => {
    const existing = [note(1, 1, 60)];
    expect(addNote(existing, CLIP, 1.5, 60, GRID)).toBeNull();
    expect(addNote(existing, CLIP, 1, 60, GRID)).toBeNull();
    expect(addNote(existing, CLIP, 1.5, 61, GRID)).not.toBeNull();
    expect(addNote([], CLIP, 4, 60, GRID)).toBeNull();
    expect(addNote([], CLIP, 5, 60, GRID)).toBeNull();
    expect(addNote([], CLIP, 1, 128, GRID)).toBeNull();
    expect(addNote([], CLIP, 1, -1, GRID)).toBeNull();
    expect(addNote([], CLIP, 1, 60, 0)).toBeNull();
  });

  it("shortens a new note at the next note on its key and at the clip's end", () => {
    const next = [note(1.5, 1, 60)];
    expect(addNote(next, CLIP, 1.25, 60, 0.5)!.find((n) => n.start === 1)!.length).toBe(0.5);
    expect(addNote(next, CLIP, 1.25, 60, 1)!.find((n) => n.start === 1)!.length).toBe(0.5);
    expect(addNote([], 1.1, 1, 60, 0.5)![0]!.length).toBeCloseTo(0.1);
  });

  it("keeps the notes sorted by start, then key", () => {
    const sorted = sortNotes([note(2, 1, 60), note(1, 1, 64), note(1, 1, 60)]);
    expect(sorted.map((n) => [n.start, n.key])).toEqual([
      [1, 60],
      [1, 64],
      [2, 60],
    ]);
    expect(noteId(note(1.5, 1, 60))).toBe("60:1.5");
  });
});

describe("moving and resizing", () => {
  const notes = [note(0, 1, 60), note(2, 1, 60)];

  it("moves by the pointer travel, snapping the start and clamping to the clip and keys", () => {
    expect(moveNote(notes, 0, 0.6, 2, 0.5, CLIP)).toEqual([note(0.5, 1, 62), note(2, 1, 60)].sort((a, b) => a.start - b.start));
    expect(moveNote(notes, 0, -3, 0, GRID, CLIP)).toBeNull(); // already at the start
    expect(moveNote(notes, 1, 5, 0, GRID, CLIP)![1]!.start).toBe(3); // ends at the clip's end
    expect(moveNote([note(0, 1, 127)], 0, 0, 5, GRID, CLIP)).toBeNull();
    expect(moveNote([note(0, 1, 0)], 0, 0, -5, GRID, CLIP)).toBeNull();
    expect(moveNote(notes, 9, 1, 0, GRID, CLIP)).toBeNull();
  });

  it("refuses a move onto another note of the same key and allows other keys", () => {
    expect(moveNote(notes, 0, 1.5, 0, GRID, CLIP)).toBeNull();
    expect(moveNote(notes, 0, 1.5, 1, GRID, CLIP)).not.toBeNull();
  });

  it("returns the result sorted, so one move is one canonical list", () => {
    const moved = moveNote(notes, 0, 3, 0, GRID, CLIP);
    // Note 0 would overlap note 1 at start 3? No: it lands at 3 (clamped to 3), after note 1.
    expect(moved!.map((n) => n.start)).toEqual([2, 3]);
  });

  it("resizes to the pointer's end, in grid steps, within the clip and before the next note", () => {
    expect(resizeNote(notes, 0, 1.6, 0.5, CLIP)![0]!.length).toBe(1.5);
    expect(resizeNote(notes, 0, 0.1, 0.5, CLIP)![0]!.length).toBe(0.5); // at least one step
    expect(resizeNote(notes, 0, 3, 0.5, CLIP)![0]!.length).toBe(2); // stops at the next note
    expect(resizeNote(notes, 1, 9, 0.5, CLIP)![1]!.length).toBe(2); // stops at the clip end
    expect(resizeNote(notes, 0, 1.1, 0.5, CLIP)).toBeNull(); // snaps to the same length
    expect(resizeNote(notes, 7, 1, 0.5, CLIP)).toBeNull();
  });

  it("removes a note by index", () => {
    expect(removeNote(notes, 0)).toEqual([notes[1]]);
    expect(removeNote(notes, 5)).toEqual(notes);
  });
});

const midiClip = (notes: Note[]): Clip => ({
  id: 7,
  track: 1,
  name: "Clip",
  start: 4,
  length: CLIP,
  muted: false,
  content: { type: "midi", notes, controllers: [{ beat: 1, controller: 1, value: 2 }] },
  take: null,
});

describe("commands", () => {
  it("replaces the clip's notes in one ReplaceClip, keeping the rest of the clip", () => {
    const clip = midiClip([note(0, 1, 60)]);
    const command = replaceNotesCommand(clip, [note(1, 1, 62)]);
    expect(command).toEqual({
      command: "replace_clip",
      clip: { ...clip, content: { type: "midi", notes: [note(1, 1, 62)], controllers: [{ beat: 1, controller: 1, value: 2 }] } },
    });
    // The original clip is untouched (the mirror is read-only).
    expect(clipNotes(clip)).toEqual([note(0, 1, 60)]);
  });

  it("refuses to edit the notes of an audio clip", () => {
    const audio: Clip = {
      ...midiClip([]),
      content: { type: "audio", media: 1, source_offset: 0, gain_db: 0, fade_in: 0, fade_out: 0 },
    };
    expect(() => replaceNotesCommand(audio, [])).toThrow();
    expect(clipNotes(audio)).toEqual([]);
  });
});
