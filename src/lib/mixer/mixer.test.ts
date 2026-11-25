import { describe, expect, it } from "vitest";
import type { ArrangeApi } from "../arrange/edits";
import { addStripInsertCommand, stripChainOf } from "../devices/chain";
import type { Bus, Device, DeviceDescriptor, EditCommand, Strip, StripSettings, Track } from "../ipc/types";
import {
  addBus,
  busReaches,
  busUsers,
  changeSend,
  dbLabel,
  newBus,
  newSend,
  outputValue,
  panLabel,
  parseOutput,
  removeBus,
  renameBus,
  routeTargets,
  sameStrip,
  setOutputCommand,
  setSendsCommand,
  setStripCommand,
  snapPan,
  stripOf,
  withSend,
  withoutSend,
} from "./mixer";

const settings = (change: Partial<StripSettings> = {}): StripSettings => ({
  gain_db: 0,
  pan: 0,
  mute: false,
  solo: false,
  solo_safe: false,
  invert_phase: false,
  output: "master",
  ...change,
});

const strip = (change: Partial<StripSettings> = {}, sends: Strip["sends"] = [], inserts: number[] = []): Strip => ({
  settings: settings(change),
  sends,
  inserts,
});

const track = (id: number, s: Strip = strip()): Track => ({
  id,
  name: `T${id}`,
  color: 0,
  kind: "audio",
  strip: s,
  instrument: null,
  input: null,
  monitor: "off",
});

const bus = (id: number, s: Strip = strip()): Bus => ({ id, name: `B${id}`, color: 0, strip: s });

/** A project's mixer: tracks 1 and 2, buses 10, 11, 12. */
function mixer(buses: Bus[], tracks: Track[] = [track(1), track(2)]) {
  return { tracks, buses, master: strip() };
}

/** An API that records what the view-model sends. */
function recorder() {
  const log: string[] = [];
  const edits: EditCommand[] = [];
  const api: ArrangeApi = {
    edit: async (edit) => (edits.push(edit), log.push(`edit:${edit.command}`)),
    begin: async (label) => log.push(`begin:${label}`),
    end: async () => log.push("end"),
    cancel: async () => log.push("cancel"),
    allocIds: async (count) => Array.from({ length: count }, (_, i) => 100 + i),
    mediaImport: async () => {
      throw new Error("unused");
    },
  };
  return { api, log, edits };
}

describe("pan and level labels", () => {
  it("shows the pan as C, L or R with a percentage", () => {
    expect(panLabel(0)).toBe("C");
    expect(panLabel(-1)).toBe("L100");
    expect(panLabel(1)).toBe("R100");
    expect(panLabel(0.254)).toBe("R25");
    expect(panLabel(-0.4)).toBe("L40");
    expect(panLabel(0.004)).toBe("C");
    expect(panLabel(7)).toBe("R100");
    expect(panLabel(Number.NaN)).toBe("C");
  });

  it("snaps a pan near the centre to it and limits the rest", () => {
    expect(snapPan(0.01)).toBe(0);
    expect(snapPan(-0.019)).toBe(0);
    expect(snapPan(0.3)).toBe(0.3);
    expect(snapPan(3)).toBe(1);
    expect(snapPan(-3)).toBe(-1);
    expect(snapPan(Number.NaN)).toBe(0);
  });

  it("reads the bottom of a fader as minus infinity", () => {
    expect(dbLabel(-60)).toBe("-inf dB");
    expect(dbLabel(-12.34)).toBe("-12.3 dB");
    expect(dbLabel(0)).toBe("0.0 dB");
  });
});

describe("strips", () => {
  it("finds the strip a reference names", () => {
    const project = mixer([bus(10, strip({ gain_db: -3 }))], [track(1, strip({ pan: 0.5 }))]);
    expect(stripOf(project, { track: 1 })?.settings.pan).toBe(0.5);
    expect(stripOf(project, { bus: 10 })?.settings.gain_db).toBe(-3);
    expect(stripOf(project, "master")).toBe(project.master);
    expect(stripOf(project, { track: 9 })).toBeUndefined();
    expect(stripOf(project, { bus: 9 })).toBeUndefined();
  });

  it("compares references", () => {
    expect(sameStrip("master", "master")).toBe(true);
    expect(sameStrip({ track: 1 }, { track: 1 })).toBe(true);
    expect(sameStrip({ track: 1 }, { bus: 1 })).toBe(false);
    expect(sameStrip({ bus: 1 }, "master")).toBe(false);
    expect(sameStrip({ bus: 2 }, { bus: 1 })).toBe(false);
  });

  it("changes one setting and keeps the rest", () => {
    const current = settings({ gain_db: -6, mute: true });
    expect(setStripCommand({ track: 1 }, current, { pan: -0.5 })).toEqual({
      command: "set_strip_settings",
      target: { track: 1 },
      settings: { ...current, pan: -0.5 },
    });
    expect(setStripCommand({ track: 1 }, current, { solo: true })).toMatchObject({
      settings: { solo: true, gain_db: -6, mute: true },
    });
  });
});

describe("routing", () => {
  // Bus 10 feeds 11 (output) and sends to 12; nothing feeds 10.
  const buses = [
    bus(10, strip({ output: { bus: 11 } }, [{ target: 12, level_db: 0, pre_fader: false }])),
    bus(11),
    bus(12),
  ];
  const project = mixer(buses);

  it("follows outputs and sends to any depth", () => {
    expect(busReaches(project, 10, 11)).toBe(true);
    expect(busReaches(project, 10, 12)).toBe(true);
    expect(busReaches(project, 11, 10)).toBe(false);
    expect(busReaches(project, 10, 10)).toBe(true);
    // A cycle already in the data does not loop forever.
    const loop = mixer([bus(1, strip({ output: { bus: 2 } })), bus(2, strip({ output: { bus: 1 } }))]);
    expect(busReaches(loop, 1, 3)).toBe(false);
  });

  it("offers every bus to a track and only cycle-free ones to a bus", () => {
    expect(routeTargets(project, { track: 1 }).map((b) => b.id)).toEqual([10, 11, 12]);
    // Nothing feeds 10 but 10 feeds 11 and 12, so 10 may go to both; 11 and 12 may not go to 10
    // (nor to a bus that 10 feeds into them).
    expect(routeTargets(project, { bus: 10 }).map((b) => b.id)).toEqual([11, 12]);
    expect(routeTargets(project, { bus: 11 }).map((b) => b.id)).toEqual([12]);
    expect(routeTargets(project, { bus: 12 }).map((b) => b.id)).toEqual([11]);
    expect(routeTargets(project, "master")).toEqual([]);
  });

  it("converts an output to a select value and back", () => {
    expect(outputValue("master")).toBe("master");
    expect(outputValue({ bus: 12 })).toBe("12");
    expect(parseOutput("master")).toBe("master");
    expect(parseOutput("12")).toEqual({ bus: 12 });
    expect(parseOutput("")).toBeNull();
    expect(parseOutput("x")).toBeNull();
    expect(parseOutput("-1")).toBeNull();
  });

  it("sets a strip's output", () => {
    expect(setOutputCommand({ track: 1 }, settings(), { bus: 10 })).toMatchObject({
      command: "set_strip_settings",
      target: { track: 1 },
      settings: { output: { bus: 10 } },
    });
  });

  it("lists what still routes to a bus", () => {
    const users = mixer(
      [bus(10), bus(11, strip({ output: { bus: 10 } })), bus(12, strip({}, [newSend(10)]))],
      [track(1, strip({ output: { bus: 10 } })), track(2)],
    );
    expect(busUsers(users, 10)).toEqual([{ track: 1 }, { bus: 11 }, { bus: 12 }]);
    expect(busUsers(users, 11)).toEqual([]);
  });
});

describe("buses", () => {
  it("builds an empty bus", () => {
    const made = newBus(5, "Bus 1", 0x123456);
    expect(made.item).toMatchObject({ id: 5, name: "Bus 1", color: 0x123456 });
    expect(made.item.strip.settings.output).toBe("master");
    expect(made.item.strip.sends).toEqual([]);
    expect(made.devices).toEqual([]);
  });

  it("adds a bus named after the count", async () => {
    const { api, edits } = recorder();
    expect(await addBus(api, 2)).toBe(100);
    expect(edits).toHaveLength(1);
    expect(edits[0]).toMatchObject({ command: "insert_bus", index: 2 });
    expect((edits[0] as { bundle: { item: Bus } }).bundle.item.name).toBe("Bus 3");
  });

  it("renames a bus only to an acceptable, different name", async () => {
    const { api, edits } = recorder();
    const target = bus(10);
    expect(await renameBus(api, target, "  Drums ")).toBe(true);
    expect(edits[0]).toEqual({ command: "set_info", target: { bus: 10 }, name: "Drums", color: 0 });
    expect(await renameBus(api, target, "B10")).toBe(false);
    expect(await renameBus(api, target, "   ")).toBe(false);
    expect(edits).toHaveLength(1);
  });

  it("reroutes what feeds a bus before deleting it, in one undo step", async () => {
    const project = mixer(
      [bus(10), bus(11, strip({ output: { bus: 10 } })), bus(12, strip({}, [newSend(10), newSend(11)]))],
      [track(1, strip({ output: { bus: 10 }, gain_db: -4 })), track(2)],
    );
    const { api, log, edits } = recorder();
    await removeBus(api, project, 10);
    expect(log[0]).toBe("begin:Delete bus");
    expect(log.at(-1)).toBe("end");
    expect(edits).toEqual([
      setOutputCommand({ track: 1 }, settings({ output: { bus: 10 }, gain_db: -4 }), "master"),
      setOutputCommand({ bus: 11 }, settings({ output: { bus: 10 } }), "master"),
      setSendsCommand({ bus: 12 }, [newSend(11)]),
      { command: "remove_bus", id: 10 },
    ]);
  });

  it("cancels the transaction when an edit fails", async () => {
    const { api, log } = recorder();
    api.edit = async () => {
      throw new Error("refused");
    };
    await expect(removeBus(api, mixer([bus(10)]), 10)).rejects.toThrow("refused");
    expect(log).toEqual(["begin:Delete bus", "cancel"]);
  });
});

describe("sends", () => {
  const sends = [newSend(10), { target: 11, level_db: -6, pre_fader: true }];

  it("adds a post-fader send at 0 dB", () => {
    expect(withSend([], 12)).toEqual([{ target: 12, level_db: 0, pre_fader: false }]);
    expect(withSend(sends, 12)).toHaveLength(3);
    expect(sends).toHaveLength(2);
  });

  it("changes one send and leaves the rest", () => {
    expect(changeSend(sends, 0, { level_db: -12 })).toEqual([
      { target: 10, level_db: -12, pre_fader: false },
      sends[1],
    ]);
    expect(changeSend(sends, 1, { pre_fader: false })[1]).toEqual({ target: 11, level_db: -6, pre_fader: false });
    expect(changeSend(sends, 5, { level_db: 1 })).toEqual(sends);
  });

  it("removes one send", () => {
    expect(withoutSend(sends, 0)).toEqual([sends[1]]);
    expect(withoutSend(sends, 9)).toEqual(sends);
  });

  it("replaces a strip's sends in one command", () => {
    expect(setSendsCommand({ track: 1 }, sends)).toEqual({
      command: "set_sends",
      target: { track: 1 },
      sends,
    });
  });
});

describe("master and bus effects", () => {
  const limiter: DeviceDescriptor = {
    kind: "builtin.limiter",
    name: "Limiter",
    instrument: false,
    params: [],
  };
  const device = (id: number): Device => ({
    id,
    kind: "builtin.eq",
    name: "EQ",
    enabled: true,
    params: {},
    state: [],
    sidechain: null,
  });

  it("lists a bus or master chain in signal order", () => {
    const devices = new Map([
      [1, device(1)],
      [2, device(2)],
    ]);
    expect(stripChainOf(strip({}, [], [2, 1]), devices).map((e) => e.device.id)).toEqual([2, 1]);
    expect(stripChainOf(strip({}, [], [2, 1]), devices).every((e) => e.role === "insert")).toBe(true);
    expect(stripChainOf(strip({}, [], [3]), devices)).toEqual([]);
  });

  it("appends an effect at the end of the master chain", () => {
    expect(addStripInsertCommand("master", strip({}, [], [4, 5]), limiter, 9)).toMatchObject({
      command: "insert_device",
      slot: { type: "insert", strip: "master", index: 2 },
      bundle: { device: { id: 9, kind: "builtin.limiter", enabled: true }, lanes: [] },
    });
    expect(addStripInsertCommand({ bus: 10 }, strip(), limiter, 9)).toMatchObject({
      slot: { strip: { bus: 10 }, index: 0 },
    });
  });
});
