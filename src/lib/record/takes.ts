import { inTransaction, type ArrangeApi } from "../arrange/edits";
import type { Clip, EditCommand } from "../ipc/types";

/** A clip that is one take of a recording. */
export type TakeClip = Clip & { take: NonNullable<Clip["take"]> };

export const isTake = (clip: Clip): clip is TakeClip => clip.take !== null;

export interface TakeGroup {
  track: number;
  group: number;
  /** The group's takes, by index. */
  takes: TakeClip[];
  /** The take that plays, if the group has one. */
  active: TakeClip | undefined;
}

/** `track:group`, the key of a take group (group ids are only unique within a track). */
export const groupKey = (track: number, group: number) => `${track}:${group}`;

/** Groups the takes among `clips` by track and group, each sorted by take index. */
export function takeGroups(clips: Iterable<Clip>): Map<string, TakeGroup> {
  const groups = new Map<string, TakeGroup>();
  for (const clip of clips) {
    if (!isTake(clip)) continue;
    const key = groupKey(clip.track, clip.take.group);
    let group = groups.get(key);
    if (!group) {
      group = { track: clip.track, group: clip.take.group, takes: [], active: undefined };
      groups.set(key, group);
    }
    group.takes.push(clip);
  }
  for (const group of groups.values()) {
    group.takes.sort((a, b) => a.take.index - b.take.index || a.id - b.id);
    group.active = group.takes.find((c) => c.take.active);
  }
  return groups;
}

/**
 * The clips a lane draws: every clip that is not a take, and each group's active take. The
 * other takes are hidden (the badge on the active take reaches them), so takes never overlap.
 * A group with no active take shows nothing.
 */
export function laneClips<T extends Clip>(clips: readonly T[]): T[] {
  const shown = new Set<number>();
  for (const group of takeGroups(clips).values()) {
    if (group.active) shown.add(group.active.id);
  }
  return clips.filter((c) => !c.take || shown.has(c.id));
}

export interface TakeBadge {
  group: number;
  /** 1-based position of the active take, "Take 2/4". */
  position: number;
  count: number;
  label: string;
  /** What the badge's choices set. */
  choices: { index: number; label: string; active: boolean }[];
}

/** The badge of a clip: null when it is not a take, or the only one left of its group. */
export function badgeFor(clip: Clip, groups: ReadonlyMap<string, TakeGroup>): TakeBadge | null {
  if (!clip.take) return null;
  const group = groups.get(groupKey(clip.track, clip.take.group));
  if (!group || group.takes.length < 2) return null;
  const at = group.takes.findIndex((c) => c.id === clip.id);
  return {
    group: group.group,
    position: at + 1,
    count: group.takes.length,
    label: `Take ${at + 1}/${group.takes.length}`,
    choices: group.takes.map((c, i) => ({
      index: c.take.index,
      label: `Take ${i + 1}`,
      active: c === clip,
    })),
  };
}

/** The take after the clip's own in its group, wrapping round (click-to-cycle). */
export function nextTakeIndex(badge: TakeBadge): number {
  const at = badge.choices.findIndex((c) => c.active);
  return badge.choices[(at + 1) % badge.choices.length]!.index;
}

export const setActiveTakeCommand = (clip: Clip, index: number): EditCommand => {
  if (!clip.take) throw new Error("the clip is not a take");
  return { command: "set_active_take", track: clip.track, group: clip.take.group, index };
};

/**
 * Makes `index` the take that plays (one undo step); does nothing when it already is. The clip
 * is any take of the group.
 */
export async function chooseTake(api: ArrangeApi, clip: Clip, index: number): Promise<boolean> {
  if (!clip.take || (clip.take.index === index && clip.take.active)) return false;
  await inTransaction(api, "Choose take", () => api.edit(setActiveTakeCommand(clip, index)));
  return true;
}

/**
 * The takes to activate before `doomed` clips are deleted so that no group is left without a
 * playing take: for each group that loses its active take and keeps others, the surviving
 * take nearest below the lost one's index (else the nearest above).
 */
export function successors(
  clips: Iterable<Clip>,
  doomed: ReadonlySet<number>,
): { clip: TakeClip; index: number }[] {
  const out: { clip: TakeClip; index: number }[] = [];
  for (const group of takeGroups(clips).values()) {
    const active = group.active;
    if (!active || !doomed.has(active.id)) continue;
    const alive = group.takes.filter((c) => !doomed.has(c.id));
    if (alive.length === 0) continue;
    const below = alive.filter((c) => c.take.index < active.take.index);
    const next = below.length > 0 ? below[below.length - 1]! : alive[0]!;
    out.push({ clip: next, index: next.take.index });
  }
  return out;
}

/**
 * Deletes clips as one undo step. When that removes a group's active take, the nearest
 * remaining take of the group takes over first, so deleting a take never silences the pass.
 */
export async function deleteClipsKeepingTakes(
  api: ArrangeApi,
  allClips: Iterable<Clip>,
  doomed: readonly Pick<Clip, "id">[],
): Promise<void> {
  if (doomed.length === 0) return;
  const ids = new Set(doomed.map((c) => c.id));
  const handovers = successors(allClips, ids);
  await inTransaction(api, doomed.length === 1 ? "Delete clip" : "Delete clips", async () => {
    for (const { clip, index } of handovers) await api.edit(setActiveTakeCommand(clip, index));
    for (const clip of doomed) await api.edit({ command: "remove_clip", id: clip.id });
  });
}

/** A take whose group has other takes: split, duplicate and trim would break the group. */
export function isLocked(clip: Clip, groups: ReadonlyMap<string, TakeGroup>): boolean {
  if (!clip.take) return false;
  return (groups.get(groupKey(clip.track, clip.take.group))?.takes.length ?? 0) >= 2;
}

/** `clips` plus the hidden takes of every take group among them, so a move keeps a group together. */
export function withSiblings(
  clips: readonly Clip[],
  groups: ReadonlyMap<string, TakeGroup>,
): Clip[] {
  const out = new Map<number, Clip>(clips.map((c) => [c.id, c]));
  for (const clip of clips) {
    if (!clip.take) continue;
    for (const sibling of groups.get(groupKey(clip.track, clip.take.group))?.takes ?? []) {
      if (!out.has(sibling.id)) out.set(sibling.id, sibling);
    }
  }
  return [...out.values()];
}

export const CHOOSE_ONE_TAKE = "Choose one take first: this clip has other takes.";
