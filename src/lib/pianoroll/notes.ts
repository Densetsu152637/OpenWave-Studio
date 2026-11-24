import type { Clip, EditCommand, Note } from "../ipc/types";
import { KEYS, snapDown, snapNearest } from "./grid";

/** Every note the roll draws has this velocity (there are no velocity lanes yet). */
export const FIXED_VELOCITY = 100;

/** Float slack when comparing beat positions. */
const EPS = 1e-9;

/** The notes ordered by start, then key, so edits produce one canonical list. */
export function sortNotes(notes: readonly Note[]): Note[] {
  return [...notes].sort((a, b) => a.start - b.start || a.key - b.key);
}

/** Identity of a note inside its clip: notes on one key never overlap, so key and start name one. */
export const noteId = (note: Pick<Note, "key" | "start">): string => `${note.key}:${note.start}`;

const sameNote = (a: Note, b: Note) =>
  a.key === b.key && a.start === b.start && a.length === b.length;

/** The first note on `key` that overlaps `[start, end)`, skipping `skip`. */
function overlapping(
  notes: readonly Note[],
  key: number,
  start: number,
  end: number,
  skip: number,
): Note | undefined {
  return notes.find(
    (n, i) => i !== skip && n.key === key && n.start < end - EPS && n.start + n.length > start + EPS,
  );
}

/** The start of the first note on `key` that begins at or after `from` (skipping `skip`). */
function nextStart(notes: readonly Note[], key: number, from: number, skip: number): number | null {
  let next: number | null = null;
  notes.forEach((n, i) => {
    if (i !== skip && n.key === key && n.start >= from - EPS && (next === null || n.start < next)) {
      next = n.start;
    }
  });
  return next;
}

/**
 * Draws a note at `beat` on `key`: it starts on the grid line at or before `beat` and is one
 * grid step long (shortened to end where the next note on the key begins, and at the clip's
 * end). Null when the spot is taken, outside the clip or has no room.
 */
export function addNote(
  notes: readonly Note[],
  clipLength: number,
  beat: number,
  key: number,
  grid: number,
): Note[] | null {
  if (!(grid > 0) || !Number.isInteger(key) || key < 0 || key >= KEYS) return null;
  const start = snapDown(Math.max(0, beat), grid);
  let end = Math.min(start + grid, clipLength);
  if (end - start < EPS) return null;
  if (overlapping(notes, key, start, start + EPS * 10, -1)) return null;
  const next = nextStart(notes, key, start, -1);
  if (next !== null) end = Math.min(end, next);
  if (end - start < EPS) return null;
  return sortNotes([...notes, { start, length: end - start, key, velocity: FIXED_VELOCITY }]);
}

/**
 * Moves note `index` by `deltaBeats` (its start snaps to the grid) and `deltaKeys`. It stays
 * inside the clip and the 128 keys. Null when it did not change or would land on another note
 * of the same key.
 */
export function moveNote(
  notes: readonly Note[],
  index: number,
  deltaBeats: number,
  deltaKeys: number,
  grid: number,
  clipLength: number,
): Note[] | null {
  const note = notes[index];
  if (!note || !(grid > 0)) return null;
  const latest = Math.max(0, clipLength - note.length);
  const start = Math.min(latest, Math.max(0, snapNearest(note.start + deltaBeats, grid)));
  const key = Math.min(KEYS - 1, Math.max(0, note.key + Math.round(deltaKeys)));
  const moved = { ...note, start, key };
  if (sameNote(moved, note)) return null;
  if (overlapping(notes, key, start, start + note.length, index)) return null;
  return sortNotes(notes.map((n, i) => (i === index ? moved : n)));
}

/**
 * Resizes note `index` so its end is at `endBeat` (snapped to the grid): at least one grid
 * step, not past the clip's end or into the next note on its key. Null when the length did not
 * change.
 */
export function resizeNote(
  notes: readonly Note[],
  index: number,
  endBeat: number,
  grid: number,
  clipLength: number,
): Note[] | null {
  const note = notes[index];
  if (!note || !(grid > 0)) return null;
  let end = Math.max(note.start + grid, snapNearest(endBeat, grid));
  end = Math.min(end, Math.max(clipLength, note.start + EPS));
  const next = nextStart(notes, note.key, note.start + EPS, index);
  if (next !== null) end = Math.min(end, next);
  const length = end - note.start;
  if (length < EPS || Math.abs(length - note.length) < EPS) return null;
  return sortNotes(notes.map((n, i) => (i === index ? { ...note, length } : n)));
}

/** The notes without note `index`. */
export function removeNote(notes: readonly Note[], index: number): Note[] {
  return notes.filter((_, i) => i !== index);
}

/** `ReplaceClip` with the clip's notes replaced (its controllers and everything else stay). */
export function replaceNotesCommand(clip: Clip, notes: readonly Note[]): EditCommand {
  if (clip.content.type !== "midi") throw new Error("not a MIDI clip");
  return {
    command: "replace_clip",
    clip: { ...clip, content: { ...clip.content, notes: [...notes] } },
  };
}

/** The notes of a MIDI clip (none for other clips). */
export function clipNotes(clip: Clip): readonly Note[] {
  return clip.content.type === "midi" ? clip.content.notes : [];
}
