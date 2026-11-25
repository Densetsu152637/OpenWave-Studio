import type { ArrangeApi } from "../arrange/edits";
import { TRACK_COLORS, importedClip, inTransaction, newAudioTrack } from "../arrange/edits";
import type { TempoMap } from "../arrange/time";
import type { Clip, LibraryEntryInfo } from "../ipc/types";
import { autoWarp, loopBeats } from "./library";

/** Where a loop is added: a track (null: a new audio track below all tracks) at a beat. */
export interface AddTarget {
  trackId: number | null;
  trackCount: number;
  start: number;
}

/**
 * Adds a library file to the project as one undo step: imports it (the backend analyses it),
 * makes the audio track when there is none to put it on, and adds the clip. A loop with a known
 * tempo and at most 32 bars follows the project tempo (`warp` follow_tempo at its own tempo,
 * the clip as long as the loop is in beats); anything else is placed as an ordinary clip.
 * Resolves with the new clip and track ids and whether the clip was warped.
 */
export async function addLibraryEntry(
  api: ArrangeApi,
  tempo: TempoMap,
  entry: Pick<LibraryEntryInfo, "path" | "name" | "bpm">,
  target: AddTarget,
): Promise<{ clipId: number; trackId: number; warped: boolean }> {
  const needTrack = target.trackId === null;
  const ids = await api.allocIds(needTrack ? 2 : 1);
  const clipId = ids[0];
  const trackId = needTrack ? ids[1] : target.trackId;
  if (clipId === undefined || trackId === undefined || trackId === null) {
    throw new Error("no ids were reserved");
  }
  let warped = false;
  await inTransaction(api, "Add loop", async () => {
    const imported = await api.mediaImport(entry.path);
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
    const name = entry.name.replace(/\.[^.]+$/, "") || entry.name;
    const clip: Clip = importedClip(clipId, trackId, name, imported, target.start, tempo);
    // The tempo in the file name wins over the one detected on import.
    const seconds = imported.frames / Math.max(1, imported.sampleRate);
    const warp = autoWarp(entry.bpm ?? imported.tempo, seconds);
    if (warp && clip.content.type === "audio") {
      clip.length = loopBeats(seconds, warp.source_bpm).beats;
      clip.content = { ...clip.content, warp };
      warped = true;
    }
    await api.edit({ command: "add_clip", clip });
  });
  return { clipId, trackId, warped };
}
