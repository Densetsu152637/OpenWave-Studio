import { describe, expect, it } from "vitest";
import { armedSet, canRecord, startRecording, toggleArm, type RecordApi } from "./record";
import { recordSetArm, recordStart } from "../ipc/commands";
import type { IpcTransport } from "../ipc/transport";

function recorder() {
  const calls: string[] = [];
  const api: RecordApi = {
    setArm: async (track, armed) => void calls.push(`arm:${track}:${armed}`),
    start: async () => void calls.push("start"),
  };
  return { api, calls };
}

describe("armed tracks", () => {
  it("come from the mirror's history and are empty before it attaches", () => {
    expect([...armedSet(null)]).toEqual([]);
    expect([...armedSet(undefined)]).toEqual([]);
    expect([...armedSet({ armedTracks: [3, 9] })]).toEqual([3, 9]);
  });

  it("toggle to the opposite of what the mirror shows", async () => {
    const { api, calls } = recorder();
    expect(await toggleArm(api, new Set<number>(), 4)).toBe(true);
    expect(await toggleArm(api, new Set([4, 5]), 4)).toBe(false);
    expect(calls).toEqual(["arm:4:true", "arm:4:false"]);
  });

  it("report a refusal to the caller", async () => {
    const api: RecordApi = {
      setArm: async () => {
        throw new Error("stop recording first");
      },
      start: async () => {},
    };
    await expect(toggleArm(api, new Set<number>(), 1)).rejects.toThrow("stop recording first");
  });
});

describe("record button", () => {
  it("is available with an armed track and no recording running", () => {
    expect(canRecord(new Set<number>(), false)).toBe(false);
    expect(canRecord(new Set([2]), false)).toBe(true);
    expect(canRecord(new Set([2]), true)).toBe(false);
  });

  it("starts the backend once when a track is armed", async () => {
    const { api, calls } = recorder();
    await startRecording(api, new Set([2]));
    expect(calls).toEqual(["start"]);
  });

  it("refuses without an armed track and never asks the backend", async () => {
    const { api, calls } = recorder();
    await expect(startRecording(api, new Set<number>())).rejects.toThrow("Arm an audio track");
    expect(calls).toEqual([]);
  });
});

describe("record commands", () => {
  it("name the backend commands with their arguments", async () => {
    const invoked: [string, Record<string, unknown> | undefined][] = [];
    const transport: IpcTransport = {
      available: true,
      channel: <T>(handler: (message: T) => void) => ({ onmessage: handler }),
      invoke: (async (command: string, args?: Record<string, unknown>) => {
        invoked.push([command, args]);
      }) as IpcTransport["invoke"],
    };
    await recordSetArm(7, true, transport);
    await recordStart(transport);
    expect(invoked).toEqual([
      ["record_set_arm", { track: 7, armed: true }],
      ["record_start", undefined],
    ]);
  });
});
