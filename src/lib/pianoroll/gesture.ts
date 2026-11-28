import type { Note } from "../ipc/types";
import { indicesOf, moveNotes, resizeNotes } from "./edit";
import { type RollView, hitTest, noteRect, pointToCell } from "./grid";
import { addNote, idsOfPicked, noteIds } from "./notes";

/** What a press on empty space does: draw a note, or start a selection rectangle. */
export type Tool = "draw" | "select";

/** A drag of the selected notes (or the grabbed one), from the press that grabbed them. */
export interface Drag {
  kind: "move" | "resize-start" | "resize-end";
  /** Ctrl/Cmd was held: a move leaves the originals and drags copies. */
  copy: boolean;
  /** The notes at the press (the drag always starts over from these). */
  notes: readonly Note[];
  /** Indices in `notes` of the notes that move together. */
  indices: readonly number[];
  /** Index of the grabbed note. */
  anchor: number;
  /** Where in the grabbed note the press landed, in beats from its start. */
  grabBeat: number;
  /** Row the press landed on. */
  grabKey: number;
  /**
   * The selection a press-and-release without movement leaves: pressing one note of a group
   * keeps the group so it can be dragged, but a plain click on it selects just that note.
   */
  clickSelection: string[] | null;
}

/** A selection rectangle in roll pixels, as it is dragged. */
export interface Marquee {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
  /** The selection before the marquee (kept when shift was held, else empty). */
  base: readonly string[];
}

/** Modifier keys of a press. */
export interface Modifiers {
  shift: boolean;
  /** Ctrl or Cmd. */
  ctrl: boolean;
}

/** What a press on the roll does. */
export type Press =
  | { type: "add"; notes: Note[]; selection: string[] }
  | { type: "drag"; drag: Drag; selection: string[] }
  | { type: "marquee"; marquee: Marquee; selection: string[] }
  | { type: "select"; selection: string[] }
  | { type: "none"; selection: string[] };

/**
 * A press at pixel `x`, `y`.
 *
 * * On a note: its left or right edge resizes, the rest moves. Plain press selects it (keeping
 *   a group it already belongs to so the group can be dragged), shift toggles it, Ctrl/Cmd
 *   with a move drags copies.
 * * On empty space with the draw tool: draws a note of one grid step there and selects it.
 * * On empty space with the select tool, or with shift: starts a selection rectangle (shift
 *   adds to the selection, otherwise the selection is cleared).
 */
export function press(
  notes: readonly Note[],
  selection: ReadonlySet<string>,
  view: RollView,
  x: number,
  y: number,
  options: { tool: Tool; grid: number; clipLength: number } & Modifiers,
): Press {
  const { tool, grid, clipLength, shift, ctrl } = options;
  const hit = hitTest(notes, view, x, y);
  const cell = pointToCell(view, x, y);
  if (hit) {
    const note = notes[hit.index]!;
    const id = noteIds(notes)[hit.index]!;
    if (shift) {
      const next = new Set(selection);
      if (next.has(id)) {
        next.delete(id);
        return { type: "select", selection: [...next] };
      }
      next.add(id);
      return {
        type: "drag",
        selection: [...next],
        drag: grab(notes, [...next], hit.index, hit.part, cell, ctrl, null),
      };
    }
    const inGroup = selection.has(id);
    const chosen = inGroup ? [...selection] : [id];
    return {
      type: "drag",
      selection: chosen,
      drag: grab(notes, chosen, hit.index, hit.part, cell, ctrl, inGroup && selection.size > 1 ? [id] : null),
    };
  }
  if (tool === "draw" && !shift) {
    const added = addNote(notes, clipLength, cell.beat, cell.key, grid);
    if (!added) return { type: "none", selection: [] };
    const note = added.find(
      (n) => n.key === cell.key && n.start <= cell.beat + 1e-9 && n.start + n.length > cell.beat - 1e-9,
    )!;
    return { type: "add", notes: added, selection: idsOfPicked(added, new Set([note])) };
  }
  const base = shift ? [...selection] : [];
  return {
    type: "marquee",
    marquee: { x0: x, y0: y, x1: x, y1: y, base },
    selection: base,
  };
}

function grab(
  notes: readonly Note[],
  selection: readonly string[],
  anchor: number,
  part: "body" | "start" | "end",
  cell: { beat: number; key: number },
  copy: boolean,
  clickSelection: string[] | null,
): Drag {
  const note = notes[anchor]!;
  const kind = part === "end" ? "resize-end" : part === "start" ? "resize-start" : "move";
  return {
    kind,
    copy: copy && kind === "move",
    notes,
    indices: indicesOf(notes, new Set(selection)),
    anchor,
    grabBeat: cell.beat - note.start,
    grabKey: cell.key,
    clickSelection,
  };
}

/**
 * The notes while the pointer is at `x`, `y` during `drag`, with the ids the selection then
 * has. Null when nothing differs from the press or the position is not valid (keep showing the
 * previous one).
 */
export function dragPreview(
  drag: Drag,
  view: RollView,
  x: number,
  y: number,
  grid: number,
  clipLength: number,
): { notes: Note[]; selected: string[] } | null {
  const cell = pointToCell(view, x, y);
  const anchor = drag.notes[drag.anchor]!;
  if (drag.kind === "resize-end") {
    return resizeNotes(drag.notes, drag.indices, drag.anchor, "end", cell.beat, grid, clipLength);
  }
  if (drag.kind === "resize-start") {
    return resizeNotes(drag.notes, drag.indices, drag.anchor, "start", cell.beat, grid, clipLength);
  }
  return moveNotes(
    drag.notes,
    drag.indices,
    drag.anchor,
    cell.beat - drag.grabBeat - anchor.start,
    cell.key - drag.grabKey,
    grid,
    clipLength,
    drag.copy,
  );
}

/** The ids of the notes that touch the rectangle (a note counts when it overlaps it at all). */
export function marqueeIds(notes: readonly Note[], view: RollView, m: Marquee): string[] {
  const left = Math.min(m.x0, m.x1);
  const right = Math.max(m.x0, m.x1);
  const top = Math.min(m.y0, m.y1);
  const bottom = Math.max(m.y0, m.y1);
  const hits = notes.filter((n) => {
    const r = noteRect(view, n);
    return r.x < right && r.x + r.width > left && r.y < bottom && r.y + r.height > top;
  });
  const picked = new Set(hits);
  return [...new Set([...m.base, ...idsOfPicked(notes, picked)])];
}

/** The marquee moved to `x`, `y`, with the selection it gives. */
export function dragMarquee(
  notes: readonly Note[],
  view: RollView,
  marquee: Marquee,
  x: number,
  y: number,
): { marquee: Marquee; selection: string[] } {
  const next = { ...marquee, x1: x, y1: y };
  return { marquee: next, selection: marqueeIds(notes, view, next) };
}
