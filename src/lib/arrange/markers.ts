import type { EditCommand, Marker } from "../ipc/types";
import { validName, type ArrangeApi } from "./edits";

/** Colour (`0xRRGGBB`) new markers get. */
export const MARKER_COLOR = 0xf2cf4b;

/** "Marker N" with the first N no marker uses. */
export function nextMarkerName(markers: Iterable<Pick<Marker, "name">>): string {
  const used = new Set([...markers].map((m) => m.name));
  let n = 1;
  while (used.has(`Marker ${n}`)) n++;
  return `Marker ${n}`;
}

export const addMarkerCommand = (marker: Marker): EditCommand => ({ command: "add_marker", marker });

export const removeMarkerCommand = (id: number): EditCommand => ({ command: "remove_marker", id });

/** Renaming replaces the marker with the same id and position (one command, one undo step). */
export const renameMarkerCommand = (marker: Marker, name: string): EditCommand => ({
  command: "replace_marker",
  marker: { ...marker, name },
});

/** Adds a marker at `beat`; resolves with its id. */
export async function addMarker(
  api: ArrangeApi,
  beat: number,
  existing: Iterable<Pick<Marker, "name">>,
): Promise<number> {
  const [id] = await api.allocIds(1);
  if (id === undefined) throw new Error("no marker id was reserved");
  await api.edit(
    addMarkerCommand({
      id,
      name: nextMarkerName(existing),
      beat: Math.max(0, beat),
      color: MARKER_COLOR,
    }),
  );
  return id;
}

/** Renames a marker; false (and nothing sent) for an unchanged or unacceptable name. */
export async function renameMarker(
  api: ArrangeApi,
  marker: Marker,
  name: string,
): Promise<boolean> {
  const next = name.trim();
  if (next === marker.name || !validName(next)) return false;
  await api.edit(renameMarkerCommand(marker, next));
  return true;
}

export const removeMarker = (api: ArrangeApi, id: number) => api.edit(removeMarkerCommand(id));
