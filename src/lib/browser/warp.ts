import type { ArrangeApi } from "../arrange/edits";
import { inTransaction } from "../arrange/edits";
import type { TempoMap } from "../arrange/time";
import type { Clip, MediaItem, Warp } from "../ipc/types";

/** Largest pitch shift of a warped clip, in semitones either way (the model's limit). */
export const MAX_TRANSPOSE = 24;

type AudioClip = Clip & { content: Extract<Clip["content"], { type: "audio" }> };

const isAudio = (clip: Clip): clip is AudioClip => clip.content.type === "audio";

/** Whether a clip follows the project tempo. */
export const isWarped = (clip: Clip): boolean =>
  isAudio(clip) && clip.content.warp?.mode === "follow_tempo";

/** `"on"` or `"off"` when every audio clip agrees, `"mixed"` when not, null without audio clips. */
export function warpState(clips: readonly Clip[]): "on" | "off" | "mixed" | null {
  const audio = clips.filter(isAudio);
  if (audio.length === 0) return null;
  const on = audio.filter(isWarped).length;
  return on === audio.length ? "on" : on === 0 ? "off" : "mixed";
}

/** The source tempo every warped clip shares, or null when they differ or none is warped. */
export function sharedSourceBpm(clips: readonly Clip[]): number | null {
  const tempi = clips
    .filter(isAudio)
    .map((c) => (c.content.warp?.mode === "follow_tempo" ? c.content.warp.source_bpm : null))
    .filter((bpm): bpm is number => bpm !== null);
  const first = tempi[0];
  return first !== undefined && tempi.every((t) => Math.abs(t - first) < 1e-6) ? first : null;
}

/** The transposition every warped clip shares, or null when they differ or none is warped. */
export function sharedTranspose(clips: readonly Clip[]): number | null {
  const values = clips
    .filter(isAudio)
    .filter(isWarped)
    .map((c) => c.content.warp?.transpose_semitones ?? 0);
  const first = values[0];
  return first !== undefined && values.every((v) => v === first) ? first : null;
}

/** A transposition the model takes: finite, within 24 semitones, rounded to a cent. */
export function clampTranspose(semitones: number): number {
  if (!Number.isFinite(semitones)) return 0;
  return Math.round(Math.max(-MAX_TRANSPOSE, Math.min(MAX_TRANSPOSE, semitones)) * 100) / 100;
}

/** The tempo a clip would be warped at: its own warp, else the one detected in its media. */
export function sourceBpmFor(clip: AudioClip, media: MediaItem | undefined): number | null {
  const own = clip.content.warp?.source_bpm;
  if (own !== undefined && own > 0) return own;
  return media?.tempo && media.tempo > 0 ? media.tempo : null;
}

/**
 * The clip with warp turned on or off. Turning it on keeps the audio the clip plays: the clip
 * becomes as long, in beats, as that audio is at the source tempo. Turning it off does the
 * reverse at the project tempo from the clip start. Throws when there is no tempo to warp from
 * (the media has none detected and the clip no warp yet).
 */
export function withWarp(
  clip: Clip,
  enabled: boolean,
  media: MediaItem | undefined,
  tempo: TempoMap,
): Clip {
  if (!isAudio(clip)) return clip;
  const current = clip.content.warp;
  if (enabled === (current?.mode === "follow_tempo")) return clip;
  const sourceBpm = sourceBpmFor(clip, media);
  if (enabled && sourceBpm === null) {
    throw new Error(
      "No tempo is known for this clip's audio, so it cannot follow the project tempo.",
    );
  }
  const bpm = sourceBpm ?? current?.source_bpm ?? 120;
  const start = tempo.beatsToSeconds(clip.start);
  let length: number;
  if (enabled) {
    // The audio plays for this many seconds; at the source tempo that is this many beats.
    length = ((tempo.beatsToSeconds(clip.start + clip.length) - start) * bpm) / 60;
  } else {
    length = tempo.secondsToBeats(start + (clip.length * 60) / bpm) - clip.start;
  }
  const warp: Warp = {
    mode: enabled ? "follow_tempo" : "off",
    source_bpm: bpm,
    markers: current?.markers ?? [],
    transpose_semitones: enabled ? (current?.transpose_semitones ?? 0) : 0,
  };
  return { ...clip, length: Math.max(1e-6, length), content: { ...clip.content, warp } };
}

/** Turns warp on or off for the audio clips among `clips`: one `ReplaceClip` each, one undo step. */
export async function setClipsWarp(
  api: ArrangeApi,
  clips: readonly Clip[],
  enabled: boolean,
  mediaOf: (id: number) => MediaItem | undefined,
  tempo: TempoMap,
): Promise<boolean> {
  const changes = clips.filter(isAudio).flatMap((clip) => {
    const next = withWarp(clip, enabled, mediaOf(clip.content.media), tempo);
    return next === clip ? [] : [next];
  });
  if (changes.length === 0) return false;
  await inTransaction(api, enabled ? "Warp clips" : "Unwarp clips", async () => {
    for (const clip of changes) await api.edit({ command: "replace_clip", clip });
  });
  return true;
}

/** Sets the transposition of the warped clips among `clips` in one undo step. */
export async function setClipsTranspose(
  api: ArrangeApi,
  clips: readonly Clip[],
  semitones: number,
): Promise<boolean> {
  const value = clampTranspose(semitones);
  const changes = clips
    .filter(isAudio)
    .filter(isWarped)
    .filter((c) => (c.content.warp?.transpose_semitones ?? 0) !== value);
  if (changes.length === 0) return false;
  await inTransaction(api, "Transpose clips", async () => {
    for (const clip of changes) {
      const warp = clip.content.warp as Warp;
      await api.edit({
        command: "replace_clip",
        clip: {
          ...clip,
          content: { ...clip.content, warp: { ...warp, transpose_semitones: value } },
        },
      });
    }
  });
  return true;
}
