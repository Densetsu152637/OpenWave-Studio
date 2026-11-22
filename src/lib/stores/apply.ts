import type {
  AutomationLane,
  Bus,
  Clip,
  Device,
  HistoryState,
  Marker,
  MediaItem,
  ModelPatch,
  Pattern,
  PatchOp,
  Project,
  ProjectSettings,
  SignatureChangeJson,
  Snapshot,
  Strip,
  TempoPointJson,
  Track,
} from "../ipc/types";

/**
 * The mirrored project: the `Project` JSON shape with the keyed collections held as maps by id
 * (the wire form is arrays in id order). Everything is read-only; the only way to change a
 * mirror is `applyPatch`, which returns a new value and never mutates the old one, so a Svelte
 * store can hand each version to its subscribers.
 */
export interface MirrorProject {
  readonly name: string;
  readonly settings: ProjectSettings;
  readonly tempo_map: readonly TempoPointJson[];
  readonly time_signature_map: readonly SignatureChangeJson[];
  readonly tracks: readonly Track[];
  readonly buses: readonly Bus[];
  readonly master: Strip;
  readonly clips: ReadonlyMap<number, Clip>;
  readonly patterns: ReadonlyMap<number, Pattern>;
  readonly media_pool: ReadonlyMap<number, MediaItem>;
  readonly devices: ReadonlyMap<number, Device>;
  readonly automation: ReadonlyMap<number, AutomationLane>;
  readonly markers: ReadonlyMap<number, Marker>;
  readonly next_id: number;
}

const byId = <T extends { id: number }>(items: readonly T[]): Map<number, T> =>
  new Map(items.map((item) => [item.id, item]));

/** Builds the mirrored form of a wire project. */
export function fromProject(project: Project): MirrorProject {
  return {
    name: project.name,
    settings: project.settings,
    tempo_map: project.tempo_map,
    time_signature_map: project.time_signature_map,
    tracks: project.tracks,
    buses: project.buses,
    master: project.master,
    clips: byId(project.clips),
    patterns: byId(project.patterns),
    media_pool: byId(project.media_pool),
    devices: byId(project.devices),
    automation: byId(project.automation),
    markers: byId(project.markers),
    next_id: project.next_id,
  };
}

const sorted = <T>(map: ReadonlyMap<number, T>): T[] =>
  [...map.entries()].sort((a, b) => a[0] - b[0]).map((entry) => entry[1]);

/** The wire form of a mirrored project (keyed collections as arrays in id order). */
export function toProject(project: MirrorProject): Project {
  return {
    name: project.name,
    settings: project.settings,
    tempo_map: [...project.tempo_map],
    time_signature_map: [...project.time_signature_map],
    tracks: [...project.tracks],
    buses: [...project.buses],
    master: project.master,
    clips: sorted(project.clips),
    patterns: sorted(project.patterns),
    media_pool: sorted(project.media_pool),
    devices: sorted(project.devices),
    automation: sorted(project.automation),
    markers: sorted(project.markers),
    next_id: project.next_id,
  };
}

/** Mutable working copy used inside one `applyPatch` call. Collections are copied on first write. */
type Draft = {
  -readonly [K in keyof MirrorProject]: MirrorProject[K] extends ReadonlyMap<number, infer V>
    ? Map<number, V>
    : MirrorProject[K] extends readonly (infer V)[]
      ? V[]
      : MirrorProject[K];
};

type MapKey = "clips" | "patterns" | "media_pool" | "devices" | "automation" | "markers";

function upsertList<T extends { id: number }>(list: T[], item: T): void {
  const at = list.findIndex((existing) => existing.id === item.id);
  if (at >= 0) list[at] = item;
  else list.push(item);
}

function reorder<T extends { id: number }>(list: T[], ids: readonly number[]): T[] {
  const rest = [...list];
  const next: T[] = [];
  for (const id of ids) {
    const at = rest.findIndex((item) => item.id === id);
    if (at >= 0) next.push(...rest.splice(at, 1));
  }
  // An id the order did not name keeps its place at the end (cannot happen with a valid patch).
  return [...next, ...rest];
}

function applyOp(draft: Draft, op: PatchOp, own: Set<string>): Draft {
  // Copies a collection the first time this patch writes to it.
  const map = <K extends MapKey>(key: K): Draft[K] => {
    if (!own.has(key)) {
      (draft[key] as Map<number, unknown>) = new Map(draft[key] as Map<number, unknown>);
      own.add(key);
    }
    return draft[key];
  };
  const list = <K extends "tracks" | "buses">(key: K): Draft[K] => {
    if (!own.has(key)) {
      (draft[key] as unknown[]) = [...(draft[key] as unknown[])];
      own.add(key);
    }
    return draft[key];
  };
  switch (op.op) {
    case "reset": {
      own.clear();
      return { ...fromProject(op.project) } as Draft;
    }
    case "set_name":
      draft.name = op.name;
      break;
    case "set_settings":
      draft.settings = op.settings;
      break;
    case "set_tempo_map":
      draft.tempo_map = op.tempo_map;
      break;
    case "set_time_signature_map":
      draft.time_signature_map = op.map;
      break;
    case "set_master":
      draft.master = op.master;
      break;
    case "set_next_id":
      draft.next_id = op.next_id;
      break;
    case "upsert_track":
      upsertList(list("tracks"), op.track);
      break;
    case "remove_track":
      draft.tracks = list("tracks").filter((t) => t.id !== op.id);
      break;
    case "order_tracks":
      draft.tracks = reorder(list("tracks"), op.ids);
      break;
    case "upsert_bus":
      upsertList(list("buses"), op.bus);
      break;
    case "remove_bus":
      draft.buses = list("buses").filter((b) => b.id !== op.id);
      break;
    case "order_buses":
      draft.buses = reorder(list("buses"), op.ids);
      break;
    case "upsert_clip":
      map("clips").set(op.clip.id, op.clip);
      break;
    case "remove_clip":
      map("clips").delete(op.id);
      break;
    case "upsert_pattern":
      map("patterns").set(op.pattern.id, op.pattern);
      break;
    case "remove_pattern":
      map("patterns").delete(op.id);
      break;
    case "upsert_media":
      map("media_pool").set(op.item.id, op.item);
      break;
    case "remove_media":
      map("media_pool").delete(op.id);
      break;
    case "upsert_device":
      map("devices").set(op.device.id, op.device);
      break;
    case "remove_device":
      map("devices").delete(op.id);
      break;
    case "upsert_lane":
      map("automation").set(op.lane.id, op.lane);
      break;
    case "remove_lane":
      map("automation").delete(op.id);
      break;
    case "upsert_marker":
      map("markers").set(op.marker.id, op.marker);
      break;
    case "remove_marker":
      map("markers").delete(op.id);
      break;
    default: {
      // Exhaustive over the generated PatchOp union: a new Rust variant fails type-checking here.
      const never: never = op;
      throw new Error(`unknown patch op ${JSON.stringify(never)}`);
    }
  }
  return draft;
}

/** Applies a patch's operations in order and returns the next mirrored project. */
export function applyOps(project: MirrorProject, ops: readonly PatchOp[]): MirrorProject {
  let draft: Draft = { ...project } as Draft;
  const own = new Set<string>();
  for (const op of ops) draft = applyOp(draft, op, own);
  return draft;
}

/** A complete mirrored state. */
export interface MirrorState {
  /** `detached` before attaching or without a backend; `syncing` while a snapshot is awaited; `ready` otherwise. */
  readonly status: "detached" | "syncing" | "ready";
  /** Sequence number of the last patch applied (or of the snapshot). */
  readonly seq: number;
  readonly project: MirrorProject | null;
  readonly history: HistoryState | null;
  /** Snapshots requested because of a gap (not counting the first attach). */
  readonly resyncs: number;
  /** Why the last attach or resync failed, if it did. */
  readonly error: string | null;
}

export const detachedState: MirrorState = {
  status: "detached",
  seq: 0,
  project: null,
  history: null,
  resyncs: 0,
  error: null,
};

/** The state a snapshot establishes. */
export function stateFromSnapshot(snapshot: Snapshot, resyncs: number): MirrorState {
  return {
    status: "ready",
    seq: snapshot.seq,
    project: fromProject(snapshot.project),
    history: snapshot.history,
    resyncs,
    error: null,
  };
}

export type PatchResult =
  | { kind: "applied"; state: MirrorState }
  /** At or below the current sequence number: already included, ignored. */
  | { kind: "stale" }
  /** Not the next patch: the mirror needs a snapshot. */
  | { kind: "gap"; expected: number; got: number };

/** Applies the next patch in sequence. A gap leaves the state untouched. */
export function applyPatch(state: MirrorState, patch: ModelPatch): PatchResult {
  if (state.project === null) return { kind: "gap", expected: state.seq + 1, got: patch.seq };
  if (patch.seq <= state.seq) return { kind: "stale" };
  if (patch.seq !== state.seq + 1) return { kind: "gap", expected: state.seq + 1, got: patch.seq };
  return {
    kind: "applied",
    state: {
      ...state,
      seq: patch.seq,
      project: applyOps(state.project, patch.ops),
      history: patch.history,
    },
  };
}
