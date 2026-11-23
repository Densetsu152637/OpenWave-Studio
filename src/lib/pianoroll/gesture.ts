import type { Note } from "../ipc/types";
import { type RollView, hitTest, pointToCell } from "./grid";
import { addNote, moveNote, noteId, resizeNote } from "./notes";

/** A drag of one existing note, from the press that grabbed it. */
export interface Drag {
  kind: "move" | "resize";
  /** The note as it was when grabbed. */
  note: Note;
  /** Index of that note in the clip's list at the press. */
  index: number;
  /** The notes at the press (the drag always starts over from these). */
  notes: readonly Note[];
  /** Where in the note the press landed, in beats from its start. */
  grabBeat: number;
  /** Row the press landed on. */
  grabKey: number;
}

/** What a press on the roll does. */
export type Press =
  | { type: "add"; notes: Note[]; selected: string }
  | { type: "drag"; drag: Drag; selected: string }
  | { type: "none" };

/**
 * A press at pixel `x`, `y`: on a note it grabs it (its right edge resizes, the rest moves); on
 * empty space it draws a new note of one grid step there; where no note fits it does nothing.
 */
export function press(
  notes: readonly Note[],
  view: RollView,
  x: number,
  y: number,
  grid: number,
  clipLength: number,
): Press {
  const hit = hitTest(notes, view, x, y);
  const cell = pointToCell(view, x, y);
  if (hit) {
    const note = notes[hit.index]!;
    return {
      type: "drag",
      selected: noteId(note),
      drag: {
        kind: hit.part === "end" ? "resize" : "move",
        note,
        index: hit.index,
        notes,
        grabBeat: cell.beat - note.start,
        grabKey: cell.key,
      },
    };
  }
  const added = addNote(notes, clipLength, cell.beat, cell.key, grid);
  if (!added) return { type: "none" };
  const note = added.find((n) => n.key === cell.key && n.start <= cell.beat + 1e-9 && n.start + n.length > cell.beat - 1e-9)!;
  return { type: "add", notes: added, selected: noteId(note) };
}

/**
 * The notes while the pointer is at `x`, `y` during `drag` (null: not a valid position, keep
 * showing the previous one; also null when nothing changed from the press).
 */
export function dragPreview(
  drag: Drag,
  view: RollView,
  x: number,
  y: number,
  grid: number,
  clipLength: number,
): Note[] | null {
  const cell = pointToCell(view, x, y);
  if (drag.kind === "resize") {
    return resizeNote(drag.notes, drag.index, cell.beat, grid, clipLength);
  }
  return moveNote(
    drag.notes,
    drag.index,
    cell.beat - drag.grabBeat - drag.note.start,
    cell.key - drag.grabKey,
    grid,
    clipLength,
  );
}

/** The id of the dragged note once the drag put it at `notes` (the id of its new position). */
export function draggedId(drag: Drag, preview: readonly Note[] | null): string {
  if (!preview) return noteId(drag.note);
  // The moved note is the one that is not in the original list; a resize keeps key and start.
  const original = new Set(drag.notes.map((n) => `${n.key}:${n.start}:${n.length}`));
  const changed = preview.find((n) => !original.has(`${n.key}:${n.start}:${n.length}`));
  return changed ? noteId(changed) : noteId(drag.note);
}
