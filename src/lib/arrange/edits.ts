import * as commands from "../ipc/commands";
import type {
  Clip,
  EditCommand,
  MediaImportReply,
  Owned,
  StripSettings,
  Track,
} from "../ipc/types";
import type { TempoMap } from "./time";

/** What the arrangement needs from the control layer; the real one is `commandsApi`. */
export interface ArrangeApi {
  edit(edit: EditCommand): Promise<unknown>;
  begin(label: string): Promise<unknown>;
  end(): Promise<unknown>;
  cancel(): Promise<unknown>;
  allocIds(count: number): Promise<number[]>;
  mediaImport(path: string): Promise<MediaImportReply>;
}

/** The app's control layer over the IPC commands. */
export const commandsApi: ArrangeApi = {
  edit: (edit) => commands.edit(edit),
  begin: (label) => commands.beginTransaction(label),
  end: () => commands.endTransaction(),
  cancel: () => commands.cancelTransaction(),
  allocIds: (count) => commands.allocIds(count),
  mediaImport: (path) => commands.mediaImport(path),
};

/** Longest name the model accepts. */
export const MAX_NAME_CHARS = 256;

/** A name the model accepts: not empty, at most 256 characters, no control characters. */
export function validName(name: string): boolean {
  return (
    name.trim().length > 0 &&
    [...name].length <= MAX_NAME_CHARS &&
    !/[\u0000-\u001f\u007f-\u009f]/.test(name)
  );
}

/**
 * Runs `body` as one undo step: begin, body, end. A failing body cancels the transaction (the
 * edits made so far are rolled back) and the error is rethrown.
 */
export async function inTransaction<T>(
  api: ArrangeApi,
  label: string,
  body: () => Promise<T>,
): Promise<T> {
  await api.begin(label);
  let result: T;
  try {
    result = await body();
  } catch (error) {
    await api.cancel().catch(() => undefined);
    throw error;
  }
  await api.end();
  return result;
}

/** The command that moves `clip` to `start`: a replacement of the clip with the new start. */
export function moveClipCommand(clip: Clip, start: number): EditCommand {
  return { command: "replace_clip", clip: { ...clip, start } };
}

/** Moves a clip: one `ReplaceClip` inside one transaction. Resolves false when nothing moved. */
export async function moveClip(api: ArrangeApi, clip: Clip, start: number): Promise<boolean> {
  if (start === clip.start) return false;
  await inTransaction(api, "Move clip", () => api.edit(moveClipCommand(clip, start)));
  return true;
}

/**
 * Moves clips by the same `delta` beats: one `ReplaceClip` each, all inside one transaction (one
 * undo step). A clip never starts before beat 0. Resolves false when nothing moved.
 */
export async function moveClips(
  api: ArrangeApi,
  clips: readonly Clip[],
  delta: number,
): Promise<boolean> {
  const moves = clips
    .map((clip) => ({ clip, start: Math.max(0, clip.start + delta) }))
    .filter(({ clip, start }) => start !== clip.start);
  if (moves.length === 0) return false;
  await inTransaction(api, moves.length === 1 ? "Move clip" : "Move clips", async () => {
    for (const { clip, start } of moves) await api.edit(moveClipCommand(clip, start));
  });
  return true;
}

export const deleteClip = (api: ArrangeApi, clip: Pick<Clip, "id">) =>
  api.edit({ command: "remove_clip", id: clip.id });

/** Deletes clips: one `RemoveClip` each inside one transaction (one undo step). */
export async function deleteClips(api: ArrangeApi, clips: readonly Pick<Clip, "id">[]): Promise<void> {
  if (clips.length === 0) return;
  if (clips.length === 1) {
    await inTransaction(api, "Delete clip", () => deleteClip(api, clips[0]!));
    return;
  }
  await inTransaction(api, "Delete clips", async () => {
    for (const clip of clips) await deleteClip(api, clip);
  });
}

/** Colors new tracks cycle through (0xRRGGBB). */
export const TRACK_COLORS = [0x4aa3ff, 0xff9f43, 0x5fd38d, 0xe56bd1, 0xf2cf4b, 0x8f7bff];

/** A new empty audio track as the `InsertTrack` bundle. */
export function newAudioTrack(id: number, name: string, color: number): Owned<Track> {
  return {
    item: {
      id,
      name,
      color,
      kind: "audio",
      strip: {
        settings: {
          gain_db: 0,
          pan: 0,
          mute: false,
          solo: false,
          solo_safe: false,
          invert_phase: false,
          output: "master",
        },
        sends: [],
        inserts: [],
      },
      instrument: null,
      input: null,
      monitor: "Off",
    },
    clips: [],
    devices: [],
    lanes: [],
  };
}

/** Appends an empty audio track named "Audio N"; resolves with its id. */
export async function addTrack(api: ArrangeApi, trackCount: number): Promise<number> {
  const [id] = await api.allocIds(1);
  if (id === undefined) throw new Error("no track id was reserved");
  await api.edit({
    command: "insert_track",
    index: trackCount,
    bundle: newAudioTrack(id, `Audio ${trackCount + 1}`, TRACK_COLORS[trackCount % TRACK_COLORS.length]!),
  });
  return id;
}

export const removeTrack = (api: ArrangeApi, id: number) =>
  api.edit({ command: "remove_track", id });

/** Renames a track; false (and nothing sent) for an unchanged or unacceptable name. */
export async function renameTrack(api: ArrangeApi, track: Track, name: string): Promise<boolean> {
  const next = name.trim();
  if (next === track.name || !validName(next)) return false;
  await api.edit({
    command: "set_info",
    target: { track: track.id },
    name: next,
    color: track.color,
  });
  return true;
}

/** Recolors a track (`0xRRGGBB`); false (and nothing sent) when it already has that color. */
export async function setTrackColor(api: ArrangeApi, track: Track, color: number): Promise<boolean> {
  if (color === track.color) return false;
  await api.edit({ command: "set_info", target: { track: track.id }, name: track.name, color });
  return true;
}

/**
 * The index an item `by` places up (negative) or down (positive) the list moves to, or null at
 * the ends of the list or when it is not in it.
 */
export function reorderTarget(ids: readonly number[], id: number, by: number): number | null {
  const from = ids.indexOf(id);
  const to = from + by;
  return from < 0 || to < 0 || to >= ids.length ? null : to;
}

/** Moves a track to index `to` (one `MoveTrack`, one undo step). */
export const moveTrack = (api: ArrangeApi, id: number, to: number) =>
  api.edit({ command: "move_track", id, to });

/** Changes a track's mute, solo, pan or fader gain (dB); the rest of its strip settings stay. */
export async function setTrackStrip(
  api: ArrangeApi,
  track: Track,
  change: Partial<Pick<StripSettings, "mute" | "solo" | "pan" | "gain_db" | "output">>,
): Promise<void> {
  await api.edit({
    command: "set_strip_settings",
    target: { track: track.id },
    settings: { ...track.strip.settings, ...change },
  });
}

/** File name without directory or extension: the clip's display name. */
export function clipNameFromPath(path: string): string {
  const file = path.split(/[\\/]/).pop() ?? path;
  const dot = file.lastIndexOf(".");
  return dot > 0 ? file.slice(0, dot) : file;
}

export interface ImportTarget {
  /** The track to place the clip on; null creates a new audio track. */
  trackId: number | null;
  /** Tracks in the project now (for the new track's position and name). */
  trackCount: number;
  /** Where the clip starts, in beats. */
  start: number;
}

/**
 * Imports an audio file and places it as a clip: reserve ids, then in one transaction
 * `media_import`, the new track (when there is no target), and `AddClip` (so one undo removes
 * all of it). Resolves with the new clip and its track.
 */
export async function importAudio(
  api: ArrangeApi,
  tempo: TempoMap,
  path: string,
  target: ImportTarget,
): Promise<{ clipId: number; trackId: number }> {
  const needTrack = target.trackId === null;
  const ids = await api.allocIds(needTrack ? 2 : 1);
  const clipId = ids[0];
  const trackId = needTrack ? ids[1] : target.trackId;
  if (clipId === undefined || trackId === undefined || trackId === null) {
    throw new Error("no ids were reserved");
  }
  await inTransaction(api, "Import audio", async () => {
    const imported = await api.mediaImport(path);
    if (needTrack) {
      await api.edit({
        command: "insert_track",
        index: target.trackCount,
        bundle: newAudioTrack(
          trackId,
          `Audio ${target.trackCount + 1}`,
          TRACK_COLORS[target.trackCount % TRACK_COLORS.length]!,
        ),
      });
    }
    await api.edit({
      command: "add_clip",
      clip: importedClip(clipId, trackId, clipNameFromPath(path), imported, target.start, tempo),
    });
  });
  return { clipId, trackId };
}

/** The audio clip that plays all of an imported file from `start`. */
export function importedClip(
  id: number,
  track: number,
  name: string,
  imported: Pick<MediaImportReply, "media" | "frames" | "sampleRate">,
  start: number,
  tempo: TempoMap,
): Clip {
  const seconds = imported.frames / Math.max(1, imported.sampleRate);
  const end = tempo.secondsToBeats(tempo.beatsToSeconds(start) + seconds);
  return {
    id,
    track,
    name,
    start,
    length: end - start,
    muted: false,
    content: {
      type: "audio",
      media: imported.media,
      source_offset: 0,
      gain_db: 0,
      fade_in: 0,
      fade_out: 0,
    },
    take: null,
  };
}
