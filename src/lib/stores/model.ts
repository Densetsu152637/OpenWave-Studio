import { ipcAttach, ipcSnapshot } from "../ipc/commands";
import { tauriTransport, type IpcTransport } from "../ipc/transport";
import type { ModelPatch, Snapshot } from "../ipc/types";
import { applyPatch, detachedState, stateFromSnapshot, type MirrorState } from "./apply";

/** Patches held while a snapshot is in flight; beyond this the mirror starts over. */
const MAX_PENDING = 10_000;
/** Consecutive resyncs that still end in a gap before giving up until the next patch. */
const MAX_RESYNC_ATTEMPTS = 3;

type Subscriber = (state: MirrorState) => void;

/**
 * A read-only mirror of the control thread's project, as a Svelte store (the `subscribe` contract,
 * so components use `$mirror`).
 *
 * It attaches once: the snapshot it receives and the patches that follow are the only things that
 * ever change it. Patches apply strictly in sequence; one at or below the current sequence is a
 * duplicate and is ignored; one beyond the next is a gap, and the mirror asks for a fresh snapshot
 * (`ipc_snapshot`), holding later patches meanwhile and replaying those the snapshot does not
 * already contain. The UI sends edits through `$lib/ipc/commands` and sees their effect only
 * when the resulting patch arrives, so it can never diverge from the model.
 */
export class ModelMirror {
  readonly #transport: IpcTransport;
  readonly #subscribers = new Set<Subscriber>();
  #state: MirrorState = detachedState;
  #pending = new Map<number, ModelPatch>();
  #resyncing: Promise<void> | null = null;
  #disposed = false;

  constructor(transport: IpcTransport = tauriTransport) {
    this.#transport = transport;
  }

  /** The current state. */
  get state(): MirrorState {
    return this.#state;
  }

  /** Svelte store contract. */
  subscribe(run: Subscriber): () => void {
    this.#subscribers.add(run);
    run(this.#state);
    return () => {
      this.#subscribers.delete(run);
    };
  }

  /** Registers the patch channel and loads the first snapshot. Without a backend it stays detached. */
  async attach(): Promise<void> {
    if (!this.#transport.available) return;
    this.#set({ ...this.#state, status: "syncing", error: null });
    try {
      const snapshot = await ipcAttach((patch) => this.receive(patch), this.#transport);
      this.#install(snapshot);
      // Patches that arrived before the snapshot and do not follow it are a gap already.
      if (this.#pending.size > 0) void this.resync();
    } catch (error) {
      this.#set({ ...this.#state, status: "detached", error: String(error) });
    }
  }

  /** Stops applying patches. */
  dispose(): void {
    this.#disposed = true;
    this.#pending.clear();
  }

  /** Requests a fresh snapshot now (also what a detected gap does). */
  resync(): Promise<void> {
    if (this.#resyncing) return this.#resyncing;
    const run = this.#runResync().finally(() => {
      this.#resyncing = null;
    });
    this.#resyncing = run;
    return run;
  }

  /** Entry point for the patch channel. Public for the channel callback and for tests. */
  receive(patch: ModelPatch): void {
    if (this.#disposed) return;
    if (this.#state.status === "syncing" || this.#resyncing) {
      this.#hold(patch);
      return;
    }
    this.#apply(patch);
  }

  #apply(patch: ModelPatch): void {
    const result = applyPatch(this.#state, patch);
    switch (result.kind) {
      case "applied":
        this.#set(result.state);
        break;
      case "stale":
        break;
      case "gap":
        this.#hold(patch);
        void this.resync();
        break;
    }
  }

  #hold(patch: ModelPatch): void {
    if (this.#pending.size >= MAX_PENDING) this.#pending.clear();
    this.#pending.set(patch.seq, patch);
  }

  async #runResync(): Promise<void> {
    for (let attempt = 0; attempt < MAX_RESYNC_ATTEMPTS; attempt++) {
      this.#set({ ...this.#state, status: "syncing", error: null });
      let snapshot: Snapshot;
      try {
        snapshot = await ipcSnapshot(this.#transport);
      } catch (error) {
        // Keep what we have; the next patch or an explicit resync() tries again.
        this.#set({ ...this.#state, status: "ready", error: String(error) });
        return;
      }
      if (this.#disposed) return;
      this.#install(snapshot, true);
      if (this.#pending.size === 0) return;
    }
    // Still gapped after several snapshots: report and wait for the next patch to retry.
    this.#pending.clear();
    this.#set({ ...this.#state, error: "the project mirror could not catch up" });
  }

  /** Makes a snapshot the state, then replays the held patches that follow it. */
  #install(snapshot: Snapshot, isResync = false): void {
    let state = stateFromSnapshot(snapshot, this.#state.resyncs + (isResync ? 1 : 0));
    const held = [...this.#pending.values()].sort((a, b) => a.seq - b.seq);
    this.#pending.clear();
    for (const patch of held) {
      const result = applyPatch(state, patch);
      if (result.kind === "applied") state = result.state;
      else if (result.kind === "gap") this.#pending.set(patch.seq, patch);
    }
    this.#set(state);
  }

  #set(state: MirrorState): void {
    this.#state = state;
    for (const run of [...this.#subscribers]) run(state);
  }
}

/** The app's mirror over the real transport. Call `attach()` once at startup. */
export const modelMirror = new ModelMirror();
