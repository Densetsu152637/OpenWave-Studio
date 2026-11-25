import type { Clip, Track } from "../../ipc/types";
import { AUDIO_EXTENSIONS } from "../dialogs";
import {
  TRACK_COLORS,
  clipNameFromPath,
  importedClip,
  inTransaction,
  newAudioTrack,
  type ArrangeApi,
} from "../edits";
import { snapBeat, type SnapResolution } from "../snap";
import type { SignatureMap, TempoMap } from "../time";
import { xToBeat, type View } from "../view";

/** Whether `path` names a file the importer takes (by extension, case-insensitive). */
export function isAudioFile(path: string): boolean {
  const dot = path.lastIndexOf(".");
  return dot >= 0 && AUDIO_EXTENSIONS.includes(path.slice(dot + 1).toLowerCase());
}

/** Where the arrangement's track area is, in the same pixels as the drop position. */
export interface DropGeometry {
  /** Left edge of the scroller. */
  left: number;
  /** Top edge of the scroller. */
  top: number;
  scrollTop: number;
  /** Width of the track headers; the lanes start after it. */
  headerPx: number;
  /** Height of the ruler and marker rows above the first track. */
  topPx: number;
  /** Height of one track row. */
  lanePx: number;
}

/** What a drop lands on: a track (null: a new audio track below all tracks) at a beat. */
export interface DropTarget {
  trackId: number | null;
  beat: number;
}

/**
 * The track and the beat under a drop at (`x`, `y`). Null above the first track (ruler and
 * markers). A MIDI track is refused (`"midi"`): audio only. Below the last track the drop creates
 * a new audio track. The beat is snapped (not by `bypass`) and never before 0.
 */
export function dropTarget(
  x: number,
  y: number,
  geometry: DropGeometry,
  view: View,
  tracks: readonly Pick<Track, "id" | "kind">[],
  signatures: SignatureMap,
  snap: SnapResolution,
  bypass = false,
): DropTarget | "midi" | null {
  const row = Math.floor(
    (y - geometry.top + geometry.scrollTop - geometry.topPx) / geometry.lanePx,
  );
  if (row < 0) return null;
  const beat = snapBeat(
    xToBeat(view, x - geometry.left - geometry.headerPx),
    snap,
    signatures,
    bypass,
  );
  const track = tracks[row];
  if (!track) return { trackId: null, beat };
  return track.kind === "audio" ? { trackId: track.id, beat } : "midi";
}

/**
 * Imports audio files dropped at `target` and places them one after another from `start`, on the
 * target track or on a new audio track (`trackId` null). One transaction, so one undo removes it
 * all. Files that are not audio are skipped. Resolves with the new clips' ids and their track, or
 * null when no file was audio.
 */
export async function importAudioFiles(
  api: ArrangeApi,
  tempo: TempoMap,
  paths: readonly string[],
  target: { trackId: number | null; trackCount: number; start: number },
): Promise<{ clipIds: number[]; trackId: number } | null> {
  const files = paths.filter(isAudioFile);
  if (files.length === 0) return null;
  const needTrack = target.trackId === null;
  const wanted = files.length + (needTrack ? 1 : 0);
  const ids = await api.allocIds(wanted);
  const trackId = needTrack ? ids[files.length] : target.trackId;
  if (ids.length < wanted || trackId === undefined || trackId === null) {
    throw new Error("no ids were reserved");
  }
  const clipIds: number[] = [];
  await inTransaction(api, files.length === 1 ? "Import audio" : "Import audio files", async () => {
    let cursor = target.start;
    for (const [i, path] of files.entries()) {
      const imported = await api.mediaImport(path);
      if (needTrack && i === 0) {
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
      const clip: Clip = importedClip(
        ids[i]!,
        trackId,
        clipNameFromPath(path),
        imported,
        cursor,
        tempo,
      );
      await api.edit({ command: "add_clip", clip });
      clipIds.push(clip.id);
      cursor = clip.start + clip.length;
    }
  });
  return { clipIds, trackId };
}
