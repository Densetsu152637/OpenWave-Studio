import { describe, expect, it } from "vitest";
import type { DeviceDescriptor, EditCommand } from "../ipc/types";
import type { ArrangeApi } from "./edits";
import { addMidiTrack, barLengthAt, createMidiClip, newMidiClip, newMidiTrack, noteBars, synthDescriptor } from "./midi";
import { SignatureMap } from "./time";

const synth: DeviceDescriptor = { kind: "builtin.synth", name: "Synth", instrument: true, params: [] };

function recorder() {
  const edits: EditCommand[] = [];
  let next = 100;
  const api: ArrangeApi = {
    edit: async (edit) => void edits.push(edit),
    begin: async () => undefined,
    end: async () => undefined,
    cancel: async () => undefined,
    allocIds: async (count) => Array.from({ length: count }, () => next++),
    mediaImport: async () => ({ media: 1, frames: 1, sampleRate: 48_000, channels: 1 }),
  };
  return { api, edits };
}

describe("MIDI tracks", () => {
  it("creates a MIDI track whose instrument is a builtin.synth carried in the same bundle", async () => {
    const { api, edits } = recorder();
    const id = await addMidiTrack(api, 2, [synth]);
    expect(id).toBe(100);
    expect(edits).toHaveLength(1);
    const insert = edits[0]!;
    if (insert.command !== "insert_track") throw new Error("expected insert_track");
    expect(insert.index).toBe(2);
    expect(insert.bundle.item).toMatchObject({ id: 100, name: "MIDI 3", kind: "midi", instrument: 101 });
    expect(insert.bundle.devices).toEqual([
      { id: 101, kind: "builtin.synth", name: "Synth", enabled: true, params: {}, state: [], sidechain: null },
    ]);
    expect(insert.bundle.clips).toEqual([]);
    // Everything else matches an audio track: unity fader, routed to the master.
    expect(insert.bundle.item.strip.settings).toMatchObject({ gain_db: 0, output: "master" });
  });

  it("needs the synth to be listed by the backend", async () => {
    const { api, edits } = recorder();
    expect(() => synthDescriptor([])).toThrow("synth");
    await expect(addMidiTrack(api, 0, [{ ...synth, kind: "builtin.eq", instrument: false }])).rejects.toThrow("synth");
    expect(edits).toEqual([]);
    expect(newMidiTrack(1, 2, "x", 0, synth).item.kind).toBe("midi");
  });
});

describe("plugin instruments", () => {
  const plugin: DeviceDescriptor = {
    kind: "clap:com.vendor.pad",
    name: "Pad",
    instrument: true,
    params: [],
    format: "clap",
    vendor: "Vendor",
  };

  it("puts a chosen plugin instrument on the new track instead of the synth", async () => {
    const { api, edits } = recorder();
    await addMidiTrack(api, 0, [synth, plugin], plugin);
    const insert = edits[0]!;
    if (insert.command !== "insert_track") throw new Error("expected insert_track");
    expect(insert.bundle.devices[0]).toMatchObject({ kind: "clap:com.vendor.pad", name: "Pad" });
    expect(insert.bundle.item).toMatchObject({ kind: "midi", instrument: 101 });
  });

  it("refuses an effect as the instrument", async () => {
    const { api, edits } = recorder();
    const effect = { ...plugin, instrument: false };
    await expect(addMidiTrack(api, 0, [synth], effect)).rejects.toThrow("not an instrument");
    expect(edits).toEqual([]);
  });
});

describe("MIDI clips", () => {
  const signatures = new SignatureMap([]);

  it("adds an empty one-bar clip at the given beat on a MIDI track", async () => {
    const { api, edits } = recorder();
    const id = await createMidiClip(api, signatures, { id: 5, kind: "midi" }, 6.5, 1);
    expect(id).toBe(100);
    expect(edits).toEqual([
      {
        command: "add_clip",
        clip: {
          id: 100,
          track: 5,
          name: "MIDI clip 2",
          start: 6.5,
          length: 4,
          muted: false,
          content: { type: "midi", notes: [], controllers: [] },
          take: null,
        },
      },
    ]);
  });

  it("uses the bar length of the time signature at the clip's start", async () => {
    const waltz = new SignatureMap([
      { bar: 0, numerator: 4, denominator: 4 },
      { bar: 2, numerator: 3, denominator: 4 },
    ]);
    expect(barLengthAt(waltz, 1)).toBe(4);
    expect(barLengthAt(waltz, 8.5)).toBe(3);
    const six = new SignatureMap([{ bar: 0, numerator: 6, denominator: 8 }]);
    expect(barLengthAt(six, 0)).toBe(3);
  });

  it("refuses an audio track and sends nothing", async () => {
    const { api, edits } = recorder();
    await expect(createMidiClip(api, signatures, { id: 5, kind: "audio" }, 0, 0)).rejects.toThrow("MIDI");
    expect(edits).toEqual([]);
    expect(newMidiClip(1, 2, "n", 0, 4).content).toEqual({ type: "midi", notes: [], controllers: [] });
  });
});

describe("clip note bars", () => {
  const note = (start: number, length: number, key: number) => ({ start, length, key, velocity: 100 });

  it("draws nothing for an empty clip", () => {
    expect(noteBars([], 24, 72)).toEqual([]);
  });

  it("puts earlier notes left and higher notes up, inside the lane", () => {
    const bars = noteBars([note(0, 1, 60), note(2, 0.5, 72)], 24, 72);
    expect(bars[0]!.x).toBe(0);
    expect(bars[0]!.width).toBe(24);
    expect(bars[1]!.x).toBe(48);
    expect(bars[1]!.width).toBe(12);
    expect(bars[1]!.y).toBeLessThan(bars[0]!.y);
    for (const bar of bars) {
      expect(bar.y).toBeGreaterThanOrEqual(0);
      expect(bar.y + bar.height).toBeLessThanOrEqual(72);
    }
  });

  it("keeps a single note a normal bar rather than filling the lane", () => {
    const [bar] = noteBars([note(0, 1, 127)], 24, 72);
    expect(bar!.height).toBeLessThanOrEqual(6);
    expect(bar!.y + bar!.height).toBeLessThanOrEqual(72);
    // Very short notes stay visible.
    expect(noteBars([note(0, 0.001, 60)], 24, 72)[0]!.width).toBe(1);
  });
});
