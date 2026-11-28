import type { Note } from "../ipc/types";
import { KEYS, snapNearest } from "./grid";
import { idsOfPicked, noteIds, sortNotes } from "./notes";

/**
 * Edits of a group of notes (the selection) as pure functions: each takes the clip's notes and
 * the indices of the notes to change and returns the new, sorted note list plus the ids of the
 * notes the selection now consists of (a moved note has a new id). Null means "nothing changed
 * or not allowed", so a gesture shows no preview and sends no edit.
 */
export interface EditResult {
  notes: Note[];
  selected: string[];
}

const EPS = 1e-9;

/** The ids a selection of `indices` of `notes` has. */
export function idsAt(notes: readonly Note[], indices: readonly number[]): string[] {
  const wanted = new Set(indices);
  return noteIds(notes).filter((_, i) => wanted.has(i));
}

/** The indices in `notes` of the notes whose ids are in `selection` (ascending). */
export function indicesOf(notes: readonly Note[], selection: ReadonlySet<string>): number[] {
  const out: number[] = [];
  noteIds(notes).forEach((id, i) => {
    if (selection.has(id)) out.push(i);
  });
  return out;
}

const overlaps = (a: Note, b: Note) =>
  a.key === b.key && a.start < b.start + b.length - EPS && b.start < a.start + a.length - EPS;

/** True when no two notes of the list overlap on one key. */
export function isClean(notes: readonly Note[]): boolean {
  const byKey = new Map<number, Note[]>();
  for (const n of notes) {
    const list = byKey.get(n.key) ?? [];
    list.push(n);
    byKey.set(n.key, list);
  }
  for (const list of byKey.values()) {
    const sorted = sortNotes(list);
    for (let i = 1; i < sorted.length; i++) {
      if (sorted[i]!.start < sorted[i - 1]!.start + sorted[i - 1]!.length - EPS) return false;
    }
  }
  return true;
}

/**
 * Moves the notes at `indices` together by the pointer's travel. `anchor` (one of them, the
 * note that was grabbed) lands on the grid; the others keep their distance to it. The group
 * stays inside the clip and the 128 keys. `copy` leaves the originals in place and moves
 * copies instead. Null when nothing moved or the result would overlap another note.
 */
export function moveNotes(
  notes: readonly Note[],
  indices: readonly number[],
  anchor: number,
  deltaBeats: number,
  deltaKeys: number,
  grid: number,
  clipLength: number,
  copy = false,
): EditResult | null {
  const picked = indices.map((i) => notes[i]).filter((n): n is Note => n !== undefined);
  const origin = notes[anchor];
  if (!origin || picked.length === 0 || !(grid > 0)) return null;
  const minStart = Math.min(...picked.map((n) => n.start));
  const maxEnd = Math.max(...picked.map((n) => n.start + n.length));
  const low = -minStart;
  const high = Math.max(low, clipLength - maxEnd);
  const beats = Math.min(high, Math.max(low, snapNearest(origin.start + deltaBeats, grid) - origin.start));
  const minKey = Math.min(...picked.map((n) => n.key));
  const maxKey = Math.max(...picked.map((n) => n.key));
  const keys = Math.min(KEYS - 1 - maxKey, Math.max(-minKey, Math.round(deltaKeys)));
  if (!copy && Math.abs(beats) < EPS && keys === 0) return null;
  const chosen = new Set(indices);
  const moved = picked.map((n) => ({ ...n, start: n.start + beats, key: n.key + keys }));
  const staying = notes.filter((_, i) => copy || !chosen.has(i));
  if (moved.some((m) => staying.some((s) => overlaps(m, s)))) return null;
  const sorted = sortNotes([...staying, ...moved]);
  return { notes: sorted, selected: idsOfPicked(sorted, new Set(moved)) };
}

/** The first start of a note on `key` at or after `from`, skipping index `skip`. */
function nextStart(notes: readonly Note[], key: number, from: number, skip: number): number | null {
  let next: number | null = null;
  notes.forEach((n, i) => {
    if (i !== skip && n.key === key && n.start >= from - EPS && (next === null || n.start < next)) {
      next = n.start;
    }
  });
  return next;
}

/** The latest end of a note on `key` that is at or before `until`, skipping index `skip`. */
function previousEnd(notes: readonly Note[], key: number, until: number, skip: number): number {
  let end = 0;
  notes.forEach((n, i) => {
    const stop = n.start + n.length;
    if (i !== skip && n.key === key && stop <= until + EPS && stop > end) end = stop;
  });
  return end;
}

/**
 * Resizes the notes at `indices` by dragging one edge of `anchor` to `beat` (snapped to the
 * grid): the others change by the same amount. A note keeps at least one grid step (or its own
 * length when that is shorter), stays inside the clip and does not run into its neighbour on
 * the same key. Null when nothing changed.
 */
export function resizeNotes(
  notes: readonly Note[],
  indices: readonly number[],
  anchor: number,
  edge: "start" | "end",
  beat: number,
  grid: number,
  clipLength: number,
): EditResult | null {
  const origin = notes[anchor];
  if (!origin || !(grid > 0) || indices.length === 0) return null;
  const target = snapNearest(beat, grid);
  const delta = edge === "end" ? target - (origin.start + origin.length) : target - origin.start;
  const chosen = new Set(indices);
  let changed = false;
  const out = notes.map((n, i) => {
    if (!chosen.has(i)) return n;
    const minimum = Math.min(n.length, grid);
    let next: Note = n;
    if (edge === "end") {
      let end = Math.max(n.start + minimum, n.start + n.length + delta);
      end = Math.min(end, Math.max(clipLength, n.start + EPS));
      const following = nextStart(notes, n.key, n.start + EPS, i);
      if (following !== null) end = Math.min(end, Math.max(following, n.start + EPS));
      if (end - n.start > EPS) next = { ...n, length: end - n.start };
    } else {
      const end = n.start + n.length;
      let start = Math.min(end - minimum, n.start + delta);
      start = Math.max(start, previousEnd(notes, n.key, n.start, i), 0);
      if (end - start > EPS) next = { ...n, start, length: end - start };
    }
    if (Math.abs(next.start - n.start) > EPS || Math.abs(next.length - n.length) > EPS) changed = true;
    return next;
  });
  if (!changed) return null;
  const resized = new Set(indices.map((i) => out[i]!));
  const sorted = sortNotes(out);
  return { notes: sorted, selected: idsOfPicked(sorted, resized) };
}

/**
 * Copies the notes at `indices` to just after the group (shifted by the group's span rounded up
 * to the grid). Copies past the clip's end are dropped, one that straddles it is cut, and one
 * that would overlap a note is skipped. Null when no copy fits. The copies become the selection.
 */
export function duplicateNotes(
  notes: readonly Note[],
  indices: readonly number[],
  grid: number,
  clipLength: number,
): EditResult | null {
  const picked = indices.map((i) => notes[i]).filter((n): n is Note => n !== undefined);
  if (picked.length === 0 || !(grid > 0)) return null;
  const first = Math.min(...picked.map((n) => n.start));
  const last = Math.max(...picked.map((n) => n.start + n.length));
  const shift = Math.max(grid, Math.ceil((last - first) / grid - EPS) * grid);
  const copies: Note[] = [];
  const all = [...notes];
  for (const n of sortNotes(picked)) {
    const start = n.start + shift;
    if (start >= clipLength - EPS) continue;
    const copy = { ...n, start, length: Math.min(n.length, clipLength - start) };
    if (copy.length < EPS || all.some((o) => overlaps(copy, o))) continue;
    copies.push(copy);
    all.push(copy);
  }
  if (copies.length === 0) return null;
  const sorted = sortNotes(all);
  return { notes: sorted, selected: idsOfPicked(sorted, new Set(copies)) };
}

/** The notes without those at `indices`. */
export function deleteNotes(notes: readonly Note[], indices: readonly number[]): Note[] {
  const gone = new Set(indices);
  return notes.filter((_, i) => !gone.has(i));
}

/** Every note id. */
export const allIds = (notes: readonly Note[]): string[] => noteIds(notes);

/**
 * Sets the velocity of the grabbed note `anchor` to `velocity` and scales the other notes at
 * `indices` by the same ratio (1 to 127). The order of `notes` is kept. Null when nothing changed.
 */
export function scaleVelocities(
  notes: readonly Note[],
  indices: readonly number[],
  anchor: number,
  velocity: number,
): Note[] | null {
  const origin = notes[anchor];
  if (!origin) return null;
  const target = Math.min(127, Math.max(1, Math.round(velocity)));
  const ratio = target / origin.velocity;
  const chosen = new Set(indices);
  let changed = false;
  const out = notes.map((n, i) => {
    if (!chosen.has(i)) return n;
    const next = i === anchor ? target : Math.min(127, Math.max(1, Math.round(n.velocity * ratio)));
    if (next !== n.velocity) changed = true;
    return next === n.velocity ? n : { ...n, velocity: next };
  });
  return changed ? out : null;
}
