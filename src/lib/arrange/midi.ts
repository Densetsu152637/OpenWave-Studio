import type { DeviceDescriptor, Clip, Note, Owned, Track } from "../ipc/types";
import { newDevice } from "../devices/params";
import { SYNTH_KIND } from "../devices/descriptors";
import { type ArrangeApi, TRACK_COLORS, newAudioTrack } from "./edits";
import type { SignatureMap } from "./time";

/**
 * MIDI tracks and clips in the arrangement: creating them (one edit each) and drawing a clip's
 * notes as bars inside its lane. The piano roll (`$lib/pianoroll`) edits the notes themselves.
 */

/**
 * A new MIDI track as the `InsertTrack` bundle: kind "midi" with `instrument` set to the new
 * device, which is carried in the bundle (so one undo removes both).
 */
export function newMidiTrack(
  id: number,
  deviceId: number,
  name: string,
  color: number,
  synth: DeviceDescriptor,
): Owned<Track> {
  const audio = newAudioTrack(id, name, color);
  return {
    ...audio,
    item: { ...audio.item, kind: "midi", instrument: deviceId },
    devices: [newDevice(synth, deviceId)],
  };
}

/** The built-in synth's descriptor, or an error when the backend does not list it. */
export function synthDescriptor(descriptors: readonly DeviceDescriptor[]): DeviceDescriptor {
  const synth = descriptors.find((d) => d.kind === SYNTH_KIND && d.instrument);
  if (!synth) throw new Error("the built-in synth is not available");
  return synth;
}

/** Appends a MIDI track named "MIDI N" with a `builtin.synth` instrument; resolves with its id. */
export async function addMidiTrack(
  api: ArrangeApi,
  trackCount: number,
  descriptors: readonly DeviceDescriptor[],
): Promise<number> {
  const synth = synthDescriptor(descriptors);
  const [id, deviceId] = await api.allocIds(2);
  if (id === undefined || deviceId === undefined) throw new Error("no ids were reserved");
  await api.edit({
    command: "insert_track",
    index: trackCount,
    bundle: newMidiTrack(
      id,
      deviceId,
      `MIDI ${trackCount + 1}`,
      TRACK_COLORS[trackCount % TRACK_COLORS.length]!,
      synth,
    ),
  });
  return id;
}

/** An empty MIDI clip `length` beats long. */
export function newMidiClip(id: number, track: number, name: string, start: number, length: number): Clip {
  return {
    id,
    track,
    name,
    start,
    length,
    muted: false,
    content: { type: "midi", notes: [], controllers: [] },
  };
}

/** Length of the bar that contains `beat` (4 beats in 4/4). */
export function barLengthAt(signatures: SignatureMap, beat: number): number {
  return signatures.barAt(beat).length;
}

/**
 * Adds an empty one-bar MIDI clip at `start` on `track`; resolves with the clip's id. The bar's
 * length is the time signature's at that position.
 */
export async function createMidiClip(
  api: ArrangeApi,
  signatures: SignatureMap,
  track: Pick<Track, "id" | "kind">,
  start: number,
  clipCount: number,
): Promise<number> {
  if (track.kind !== "midi") throw new Error("MIDI clips go on MIDI tracks");
  const [id] = await api.allocIds(1);
  if (id === undefined) throw new Error("no clip id was reserved");
  await api.edit({
    command: "add_clip",
    clip: newMidiClip(id, track.id, `MIDI clip ${clipCount + 1}`, start, barLengthAt(signatures, start)),
  });
  return id;
}

export interface NoteBar {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Smallest key span a lane shows, so a one-note clip does not stretch to the lane's height. */
const MIN_KEY_SPAN = 12;
const PAD_PX = 3;
const MAX_ROW_PX = 6;

/**
 * Simple bars for a MIDI clip's notes inside a lane `height` pixels tall, relative to the
 * clip's left edge: x from the note's start, y from its key within the clip's own key range.
 * Empty for a clip without notes.
 */
export function noteBars(notes: readonly Note[], pxPerBeat: number, height: number): NoteBar[] {
  if (notes.length === 0) return [];
  const keys = notes.map((n) => n.key);
  const low = Math.min(...keys);
  const high = Math.max(...keys);
  const span = Math.max(MIN_KEY_SPAN, high - low + 1);
  // The key shown on the top row: the notes' range is centred in the span's rows.
  const top = high + Math.floor((span - (high - low + 1)) / 2);
  const row = Math.min(MAX_ROW_PX, Math.max(1, (height - 2 * PAD_PX) / span));
  return notes.map((n) => ({
    x: n.start * pxPerBeat,
    y: PAD_PX + (top - n.key) * row,
    width: Math.max(1, n.length * pxPerBeat),
    height: Math.max(1, row - 0.5),
  }));
}
