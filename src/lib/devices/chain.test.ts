import { describe, expect, it } from "vitest";
import {
  addInsert,
  addInsertCommand,
  chainOf,
  effectDescriptors,
  removeInsertCommand,
  shownEntry,
  toggleBypassCommand,
} from "./chain";
import { newDevice } from "./params";
import type { ArrangeApi } from "../arrange/edits";
import type { Device, DeviceDescriptor, EditCommand, Track } from "../ipc/types";

const descriptor = (kind: string, name: string, instrument: boolean): DeviceDescriptor => ({
  kind,
  name,
  instrument,
  params: [],
});
const synth = descriptor("builtin.synth", "Synth", true);
const eq = descriptor("builtin.eq", "EQ", false);
const reverb = descriptor("builtin.reverb", "Reverb", false);

const track = (instrument: number | null, inserts: number[]): Track =>
  ({ id: 1, instrument, strip: { inserts } }) as unknown as Track;

const devices = (...list: Device[]) => new Map(list.map((d) => [d.id, d]));

describe("device chain", () => {
  it("lists the instrument, then the inserts in signal order", () => {
    const map = devices(newDevice(synth, 10), newDevice(reverb, 12), newDevice(eq, 11));
    const chain = chainOf(track(10, [11, 12]), map);
    expect(chain.map((e) => [e.device.id, e.role])).toEqual([
      [10, "instrument"],
      [11, "insert"],
      [12, "insert"],
    ]);
    // An audio track has inserts only; ids the project lacks are skipped.
    expect(chainOf(track(null, [12, 99, 11]), map).map((e) => e.device.id)).toEqual([12, 11]);
    expect(chainOf(track(null, []), map)).toEqual([]);
  });

  it("offers only effects to add", () => {
    expect(effectDescriptors([synth, eq, reverb]).map((d) => d.kind)).toEqual([
      "builtin.eq",
      "builtin.reverb",
    ]);
  });

  it("shows the selected device, else the first, else none", () => {
    const map = devices(newDevice(synth, 10), newDevice(eq, 11));
    const chain = chainOf(track(10, [11]), map);
    expect(shownEntry(chain, 11)?.device.id).toBe(11);
    expect(shownEntry(chain, null)?.device.id).toBe(10);
    expect(shownEntry(chain, 77)?.device.id).toBe(10);
    expect(shownEntry([], 11)).toBeUndefined();
  });
});

describe("chain edits", () => {
  it("adds an insert at the end of the track's chain with default parameters", () => {
    expect(addInsertCommand(track(null, [5, 6]), eq, 20)).toEqual({
      command: "insert_device",
      slot: { type: "insert", strip: { track: 1 }, index: 2 },
      bundle: {
        device: {
          id: 20,
          kind: "builtin.eq",
          name: "EQ",
          enabled: true,
          params: {},
          state: [],
          sidechain: null,
        },
        lanes: [],
      },
    });
  });

  it("removes and bypasses by device id", () => {
    const device = newDevice(eq, 11);
    expect(removeInsertCommand(device)).toEqual({ command: "remove_device", id: 11 });
    expect(toggleBypassCommand(device)).toEqual({
      command: "set_device_enabled",
      device: 11,
      enabled: false,
    });
    expect(toggleBypassCommand({ ...device, enabled: false })).toMatchObject({ enabled: true });
  });

  it("reserves an id, sends one edit and resolves with the id", async () => {
    const edits: EditCommand[] = [];
    const api = {
      allocIds: async (count: number) => Array.from({ length: count }, () => 42),
      edit: async (edit: EditCommand) => void edits.push(edit),
    } as unknown as ArrangeApi;
    expect(await addInsert(api, track(null, []), reverb)).toBe(42);
    expect(edits).toHaveLength(1);
    expect(edits[0]).toMatchObject({ command: "insert_device", bundle: { device: { id: 42 } } });
  });

  it("refuses an instrument and an empty reservation", async () => {
    const api = { allocIds: async () => [], edit: async () => {} } as unknown as ArrangeApi;
    await expect(addInsert(api, track(null, []), synth)).rejects.toThrow("instrument");
    await expect(addInsert(api, track(null, []), eq)).rejects.toThrow("no id");
  });
});
