/** The selected clips (by id). Plain sets, replaced rather than mutated. */
export type ClipSelection = ReadonlySet<number>;

export const NO_SELECTION: ClipSelection = new Set();

/**
 * A press on clip `id`. Without a modifier it becomes the only selected clip, unless it is
 * already selected (so a drag moves the whole selection); with Shift or Ctrl/Cmd (`additive`)
 * it is toggled in or out.
 */
export function pressClip(selection: ClipSelection, id: number, additive: boolean): ClipSelection {
  if (additive) {
    const next = new Set(selection);
    if (!next.delete(id)) next.add(id);
    return next;
  }
  return selection.has(id) ? selection : new Set([id]);
}

/**
 * The release after a press: a plain click (no drag, no modifier) on a clip inside a larger
 * selection narrows the selection to that clip.
 */
export function releaseClip(
  selection: ClipSelection,
  id: number,
  additive: boolean,
  moved: boolean,
): ClipSelection {
  return !additive && !moved && selection.has(id) && selection.size > 1 ? new Set([id]) : selection;
}

/** Every clip selected (Ctrl/Cmd+A). */
export const selectAll = (ids: Iterable<number>): ClipSelection => new Set(ids);

/** The selection without clips that no longer exist (undo, delete, open); same set if intact. */
export function pruneSelection(
  selection: ClipSelection,
  exists: (id: number) => boolean,
): ClipSelection {
  for (const id of selection) {
    if (!exists(id)) return new Set([...selection].filter(exists));
  }
  return selection;
}
