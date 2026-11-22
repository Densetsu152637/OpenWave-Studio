import { describe, expect, it, vi } from "vitest";
import type { IpcTransport } from "../ipc/transport";
import type { ModelPatch, Project, Snapshot } from "../ipc/types";
import fixtureJson from "./fixtures/mirror-stream.json";
import { applyPatch, stateFromSnapshot, toProject, type MirrorState } from "./apply";
import { ModelMirror } from "./model";

/**
 * The mirror equals the model. The fixture is produced by `cargo test -p ow-ipc export_bindings`:
 * a snapshot and the ordered patch stream a real control-thread Session emitted for a random
 * command sequence (undo, redo, transactions and refused commands included), plus the session
 * state after every eighth patch and at the end. Everything here runs against a mocked `invoke`.
 */
interface Fixture {
  initial: Snapshot;
  patches: ModelPatch[];
  checkpoints: Snapshot[];
  last: Snapshot;
}
const fixture = fixtureJson as unknown as Fixture;

interface Mock {
  transport: IpcTransport;
  /** Delivers a patch on the channel registered by ipc_attach. */
  deliver(patch: ModelPatch): void;
  snapshotCalls: number;
}

/** A mocked backend: `snapshot` answers ipc_attach and every ipc_snapshot. */
function mockBackend(snapshot: (call: number) => Snapshot, attach: Snapshot = fixture.initial): Mock {
  let onmessage: (patch: ModelPatch) => void = () => {};
  const mock: Mock = {
    snapshotCalls: 0,
    deliver: (patch) => onmessage(patch),
    transport: {
      available: true,
      channel: <T>(handler: (message: T) => void) => ({ onmessage: handler }),
      invoke: vi.fn(async (command: string, args?: Record<string, unknown>) => {
        if (command === "ipc_attach") {
          onmessage = (args!.patches as { onmessage: typeof onmessage }).onmessage;
          return structuredClone(attach);
        }
        if (command === "ipc_snapshot") {
          mock.snapshotCalls += 1;
          return structuredClone(snapshot(mock.snapshotCalls));
        }
        throw new Error(`unexpected command ${command}`);
      }) as IpcTransport["invoke"],
    },
  };
  return mock;
}

const lastProject = (): Project => fixture.last.project;

function expectMirrors(state: MirrorState, snapshot: Snapshot): void {
  expect(state.status).toBe("ready");
  expect(state.seq).toBe(snapshot.seq);
  expect(toProject(state.project!)).toEqual(snapshot.project);
  expect(state.history).toEqual(snapshot.history);
}

/** The project after patches 1..seq, by folding the same patches over the initial snapshot. */
function referenceStates(): MirrorState[] {
  const states: MirrorState[] = [stateFromSnapshot(fixture.initial, 0)];
  for (const patch of fixture.patches) {
    const result = applyPatch(states[states.length - 1], patch);
    if (result.kind !== "applied") throw new Error(`reference replay failed at ${patch.seq}`);
    states.push(result.state);
  }
  return states;
}

describe("fixture", () => {
  it("is a real stream: consecutive sequence numbers, many operations, undo state", () => {
    const seqs = fixture.patches.map((p) => p.seq);
    expect(seqs).toEqual(seqs.map((_, i) => i + 1));
    expect(seqs.length).toBeGreaterThanOrEqual(40);
    const kinds = new Set(fixture.patches.flatMap((p) => p.ops.map((o) => o.op)));
    expect(kinds.size).toBeGreaterThanOrEqual(8);
    expect(fixture.patches.some((p) => p.history.canRedo)).toBe(true);
    expect(fixture.checkpoints.length).toBeGreaterThanOrEqual(4);
    expect(fixture.last.seq).toBe(seqs.length);
  });
});

describe("model mirror", () => {
  it("starts from the attach snapshot and applies patches in order until it equals the model", async () => {
    const mock = mockBackend(() => fixture.last);
    const mirror = new ModelMirror(mock.transport);
    await mirror.attach();
    expectMirrors(mirror.state, fixture.initial);
    for (const patch of fixture.patches) mock.deliver(patch);
    expectMirrors(mirror.state, fixture.last);
    expect(mirror.state.resyncs).toBe(0);
    expect(mock.snapshotCalls).toBe(0);
    expect(toProject(mirror.state.project!)).toEqual(lastProject());
  });

  it("stays equal to the model after every single patch", async () => {
    const refs = referenceStates();
    const mock = mockBackend(() => fixture.last);
    const mirror = new ModelMirror(mock.transport);
    await mirror.attach();
    fixture.patches.forEach((patch, i) => {
      mock.deliver(patch);
      expect(mirror.state.seq).toBe(i + 1);
      expect(toProject(mirror.state.project!)).toEqual(toProject(refs[i + 1].project!));
    });
  });

  it("detects a forced sequence gap, resyncs from a snapshot and carries on", async () => {
    // Lose patch k where k + 1 is a checkpoint: the snapshot the backend answers with is the
    // session state at the moment the UI saw patch k + 1.
    const gaps = fixture.checkpoints.filter((c) => c.seq >= 2 && c.seq < fixture.last.seq);
    expect(gaps.length).toBeGreaterThanOrEqual(3);
    for (const checkpoint of gaps) {
      const mock = mockBackend(() => checkpoint);
      const mirror = new ModelMirror(mock.transport);
      await mirror.attach();
      for (const patch of fixture.patches) {
        if (patch.seq === checkpoint.seq - 1) continue; // lost on the way
        mock.deliver(patch);
        if (patch.seq === checkpoint.seq) {
          expect(mirror.state.status).toBe("syncing");
          await mirror.resync();
        }
      }
      expect(mock.snapshotCalls).toBe(1);
      expect(mirror.state.resyncs).toBe(1);
      expectMirrors(mirror.state, fixture.last);
    }
  });

  it("ignores patches the resync snapshot already contains and replays the ones after it", async () => {
    // Lose patch 3; the snapshot answered later than the gap was seen (at the final state).
    const mock = mockBackend(() => fixture.last);
    const mirror = new ModelMirror(mock.transport);
    await mirror.attach();
    for (const patch of fixture.patches) if (patch.seq !== 3) mock.deliver(patch);
    await mirror.resync();
    expect(mock.snapshotCalls).toBe(1);
    expectMirrors(mirror.state, fixture.last);
  });

  it("holds patches that overtake the first snapshot and applies those that follow it", async () => {
    // The channel delivers patches 1..5 before the attach reply with the initial snapshot arrives.
    let release: (snapshot: Snapshot) => void = () => {};
    const mock = mockBackend(() => fixture.last);
    const original = mock.transport.invoke;
    mock.transport.invoke = vi.fn(async (command: string, args?: Record<string, unknown>) => {
      if (command === "ipc_attach") {
        const reply = await original(command, args);
        return new Promise<Snapshot>((resolve) => {
          release = resolve;
          void reply;
        });
      }
      return original(command, args);
    }) as IpcTransport["invoke"];
    const mirror = new ModelMirror(mock.transport);
    const attached = mirror.attach();
    await Promise.resolve();
    await Promise.resolve();
    for (const patch of fixture.patches.slice(0, 5)) mock.deliver(patch);
    expect(mirror.state.status).toBe("syncing");
    release(fixture.initial);
    await attached;
    expect(mirror.state.seq).toBe(5);
    for (const patch of fixture.patches.slice(5)) mock.deliver(patch);
    expectMirrors(mirror.state, fixture.last);
  });

  it("ignores duplicates and never goes backwards", async () => {
    const mock = mockBackend(() => fixture.last);
    const mirror = new ModelMirror(mock.transport);
    await mirror.attach();
    for (const patch of fixture.patches) {
      mock.deliver(patch);
      mock.deliver(patch);
      mock.deliver(fixture.patches[0]);
    }
    expectMirrors(mirror.state, fixture.last);
    expect(mock.snapshotCalls).toBe(0);
  });

  it("never exposes a state that differs from the model at its sequence number (random loss, reordering, duplicates)", async () => {
    const refs = referenceStates();
    const refAt = (seq: number): Snapshot =>
      seq === 0 ? fixture.initial : { seq, project: toProject(refs[seq].project!), history: refs[seq].history! };
    for (let seed = 1; seed <= 25; seed++) {
      let rng = seed * 2654435761;
      const random = () => {
        rng = (Math.imul(rng, 1664525) + 1013904223) >>> 0;
        return rng / 2 ** 32;
      };
      let highest = 0;
      const mock = mockBackend(() => refAt(highest));
      const mirror = new ModelMirror(mock.transport);
      await mirror.attach();
      // A delivery order with losses, duplicates and local reordering.
      const order: ModelPatch[] = [];
      for (const patch of fixture.patches) {
        if (random() < 0.12) continue;
        order.push(patch);
        if (random() < 0.15) order.push(patch);
      }
      for (let i = 0; i + 1 < order.length; i++) {
        if (random() < 0.1) [order[i], order[i + 1]] = [order[i + 1], order[i]];
      }
      let lastSeq = 0;
      for (const patch of order) {
        highest = Math.max(highest, patch.seq);
        mock.deliver(patch);
        if (mirror.state.status === "syncing") await mirror.resync();
        const state = mirror.state;
        expect(state.seq).toBeGreaterThanOrEqual(lastSeq);
        lastSeq = state.seq;
        expect(toProject(state.project!)).toEqual(toProject(refs[state.seq].project!));
      }
      // Anything still missing is fetched by a resync at the end.
      highest = fixture.last.seq;
      if (mirror.state.seq !== fixture.last.seq) await mirror.resync();
      expectMirrors(mirror.state, fixture.last);
    }
  });

  it("never mutates a published state", async () => {
    const mock = mockBackend(() => fixture.last);
    const mirror = new ModelMirror(mock.transport);
    await mirror.attach();
    const seen: { state: MirrorState; json: string }[] = [];
    for (const patch of fixture.patches.slice(0, 30)) {
      mock.deliver(patch);
      seen.push({ state: mirror.state, json: JSON.stringify(toProject(mirror.state.project!)) });
    }
    for (const { state, json } of seen) expect(JSON.stringify(toProject(state.project!))).toBe(json);
  });

  it("notifies subscribers with each new state and stays detached without a backend", async () => {
    const mock = mockBackend(() => fixture.last);
    const mirror = new ModelMirror(mock.transport);
    const seqs: number[] = [];
    const stop = mirror.subscribe((s) => seqs.push(s.seq));
    await mirror.attach();
    mock.deliver(fixture.patches[0]);
    stop();
    mock.deliver(fixture.patches[1]);
    expect(seqs.at(-1)).toBe(1);

    const offline = new ModelMirror({ ...mock.transport, available: false });
    await offline.attach();
    expect(offline.state.status).toBe("detached");
    expect(offline.state.project).toBeNull();
  });

  it("reports a failed snapshot and recovers on the next resync", async () => {
    let fail = true;
    const mock = mockBackend(() => {
      if (fail) throw new Error("backend busy");
      return fixture.last;
    });
    const mirror = new ModelMirror(mock.transport);
    await mirror.attach();
    mock.deliver(fixture.patches[2]); // gap: patches 1 and 2 are missing
    await mirror.resync();
    expect(mirror.state.error).toContain("backend busy");
    fail = false;
    await mirror.resync();
    expect(mirror.state.error).toBeNull();
    expectMirrors(mirror.state, fixture.last);
  });
});
