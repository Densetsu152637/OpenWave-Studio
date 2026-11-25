import { describe, expect, it } from "vitest";
import type { ArrangeApi } from "../arrange/edits";
import { SignatureMap } from "../arrange/time";
import type { Clip, EditCommand, ProjectSettings } from "../ipc/types";
import {
  badgeFor,
  chooseTake,
  commitRange,
  countInOptions,
  defaultLoop,
  deleteClipsKeepingTakes,
  dragRange,
  fitsDevice,
  grabAt,
  inputChannelCount,
  inputOptions,
  isLocked,
  withSiblings,
  onBar,
  laneClips,
  monitorView,
  nextMonitor,
  nextTakeIndex,
  stopNotice,
  successors,
  takeGroups,
  withCountIn,
  withRange,
} from "./index";

const signatures = new SignatureMap([]);
const ctx = (snap: "off" | "bar" | "1" = "1", bypass = false) => ({ signatures, snap, bypass });

function recorder() {
  const calls: string[] = [];
  const edits: EditCommand[] = [];
  const api: ArrangeApi = {
    edit: async (e) => void (edits.push(e), calls.push(e.command)),
    begin: async (l) => void calls.push(`begin:${l}`),
    end: async () => void calls.push("end"),
    cancel: async () => void calls.push("cancel"),
    allocIds: async () => [],
    mediaImport: async () => {
      throw new Error("unused");
    },
  };
  return { api, calls, edits };
}

const settings: ProjectSettings = {
  sample_rate: 48000,
  loop_range: null,
  count_in_bars: 0,
  punch_range: null,
  punch_enabled: false,
};

function take(id: number, group: number, index: number, active: boolean, track = 1): Clip {
  return {
    id,
    track,
    name: `t${id}`,
    start: 0,
    length: 8,
    muted: false,
    content: { type: "audio", media: 1, offset_frames: 0, gain_db: 0, fade_in: 0, fade_out: 0 },
    take: { group, index, active },
  } as unknown as Clip;
}

describe("input options", () => {
  it("lists default, mono channels and adjacent pairs", () => {
    const labels = inputOptions(4).map((o) => o.label);
    expect(labels).toEqual(["Default", "Mono 1", "Mono 2", "Mono 3", "Mono 4", "Stereo 1-2", "Stereo 3-4"]);
  });
  it("drops the unpaired last channel and handles no device", () => {
    expect(inputOptions(3).map((o) => o.label)).toEqual(["Default", "Mono 1", "Mono 2", "Mono 3", "Stereo 1-2"]);
    expect(inputOptions(null).map((o) => o.id)).toEqual(["default"]);
  });
  it("keeps a track's input the device lacks", () => {
    const options = inputOptions(2, { channels: { mono: 5 } });
    expect(options.at(-1)).toMatchObject({ id: "m5", label: "Mono 6 (not on this device)" });
    expect(fitsDevice({ stereo: 2 }, 3)).toBe(false);
    expect(fitsDevice({ stereo: 2 }, 4)).toBe(true);
  });
  it("maps options to model channels (0-based)", () => {
    const stereo = inputOptions(4).find((o) => o.id === "s2");
    expect(stereo?.input).toEqual({ channels: { stereo: 2 } });
  });
});

describe("monitor", () => {
  it("cycles off, auto, on", () => {
    expect(nextMonitor("off")).toBe("auto");
    expect(nextMonitor("auto")).toBe("on");
    expect(nextMonitor("on")).toBe("off");
    expect(monitorView("off").lit).toBe(false);
    expect(monitorView("on").lit).toBe(true);
  });
});

describe("range drags", () => {
  it("creates a range snapped to the grid in either direction", () => {
    const drag = { grab: "create" as const, origin: null, anchor: 4.2 };
    expect(dragRange(drag, 8.6, ctx("1"))).toEqual({ start: 4, end: 9 });
    expect(dragRange(drag, 1.1, ctx("1"))).toEqual({ start: 1, end: 4 });
    expect(dragRange(drag, 4.3, ctx("1"))).toBeNull();
    expect(dragRange(drag, 6.5, ctx("bar"))).toEqual({ start: 4, end: 8 });
  });
  it("Alt skips the grid", () => {
    const drag = { grab: "create" as const, origin: null, anchor: 1.25 };
    expect(dragRange(drag, 2.5, ctx("1", true))).toEqual({ start: 1.25, end: 2.5 });
  });
  it("moves an end but never past the other", () => {
    const origin = { start: 4, end: 8 };
    expect(dragRange({ grab: "start", origin, anchor: 4 }, 2.2, ctx())).toEqual({ start: 2, end: 8 });
    const crossed = dragRange({ grab: "start", origin, anchor: 4 }, 12, ctx())!;
    expect(crossed.start).toBeLessThan(crossed.end);
    expect(dragRange({ grab: "end", origin, anchor: 8 }, 2, ctx())!.end).toBeGreaterThan(4);
    expect(dragRange({ grab: "start", origin, anchor: 4 }, -3, ctx())!.start).toBe(0);
  });
  it("moves the body keeping its length and stays after beat 0", () => {
    const origin = { start: 4, end: 8 };
    expect(dragRange({ grab: "move", origin, anchor: 5 }, 7.1, ctx())).toEqual({ start: 6, end: 10 });
    expect(dragRange({ grab: "move", origin, anchor: 5 }, -10, ctx())).toEqual({ start: 0, end: 4 });
  });
  it("picks the grab from the pointer", () => {
    const range = { start: 1, end: 2 };
    expect(grabAt(null, 50, 0, 0)).toBe("create");
    expect(grabAt(range, 102, 100, 200)).toBe("start");
    expect(grabAt(range, 197, 100, 200)).toBe("end");
    expect(grabAt(range, 150, 100, 200)).toBe("move");
    expect(grabAt(range, 250, 100, 200)).toBe("create");
  });
  it("a new punch range turns punching on; clearing turns it off", () => {
    const set = withRange(settings, "punch", { start: 1, end: 3 });
    expect(set.punch_enabled).toBe(true);
    expect(withRange({ ...set, punch_enabled: false }, "punch", { start: 2, end: 3 }).punch_enabled).toBe(false);
    expect(withRange(set, "punch", null)).toMatchObject({ punch_range: null, punch_enabled: false });
    expect(withRange(settings, "loop", { start: 0, end: 4 }).loop_range).toEqual({ start: 0, end: 4 });
  });
  it("commits a gesture as one transaction, and skips a no-op", async () => {
    const { api, calls, edits } = recorder();
    expect(await commitRange(api, settings, "loop", { start: 0, end: 8 })).toBe(true);
    expect(calls).toEqual(["begin:Set loop range", "set_settings", "end"]);
    expect(edits[0]).toMatchObject({ settings: { loop_range: { start: 0, end: 8 } } });
    const again = recorder();
    const same = { ...settings, loop_range: { start: 0, end: 8 } };
    expect(await commitRange(again.api, same, "loop", { start: 0, end: 8 })).toBe(false);
    expect(again.calls).toEqual([]);
  });
  it("builds a default loop of four bars and clamps count-in", () => {
    expect(defaultLoop(5, signatures)).toEqual({ start: 4, end: 20 });
    expect(withCountIn(settings, 99).count_in_bars).toBe(16);
    expect(countInOptions(2)).toEqual([0, 1, 2, 4]);
    expect(countInOptions(3)).toEqual([0, 1, 2, 3, 4]);
  });
});

describe("takes", () => {
  const clips = [
    take(10, 10, 0, false),
    take(11, 10, 1, false),
    take(12, 10, 2, true),
    take(13, 10, 3, false),
    take(20, 5, 0, true, 2),
  ];
  const groups = takeGroups(clips);

  it("shows only each group's active take on the lane", () => {
    expect(laneClips(clips).map((c) => c.id)).toEqual([12, 20]);
    const plain = { ...take(30, 0, 0, true), take: null } as Clip;
    expect(laneClips([plain, ...clips]).map((c) => c.id)).toEqual([30, 12, 20]);
  });
  it("shows nothing for a group left without an active take", () => {
    expect(laneClips([take(1, 1, 3, false), take(2, 1, 4, false)])).toEqual([]);
  });
  it("locks a take with siblings and moves its whole group together", () => {
    const group = [take(1, 1, 0, false), take(2, 1, 1, true), take(3, 7, 0, true)];
    const groups = takeGroups(group);
    expect(isLocked(group[1]!, groups)).toBe(true);
    expect(isLocked(group[2]!, groups)).toBe(false);
    expect(isLocked({ ...group[2]!, take: null } as Clip, groups)).toBe(false);
    expect(withSiblings([group[1]!], groups).map((c) => c.id).sort()).toEqual([1, 2]);
    expect(withSiblings([group[1]!, group[0]!, group[2]!], groups)).toHaveLength(3);
  });
  it("badges the active take with its position and choices", () => {
    const badge = badgeFor(clips[2]!, groups)!;
    expect(badge.label).toBe("Take 3/4");
    expect(badge.choices.map((c) => c.index)).toEqual([0, 1, 2, 3]);
    expect(nextTakeIndex(badge)).toBe(3);
    expect(nextTakeIndex(badgeFor(clips[3]!, groups)!)).toBe(0);
    expect(badgeFor(clips[4]!, groups)).toBeNull();
  });
  it("chooses a take in one undo step", async () => {
    const { api, calls, edits } = recorder();
    expect(await chooseTake(api, clips[2]!, 1)).toBe(true);
    expect(calls).toEqual(["begin:Choose take", "set_active_take", "end"]);
    expect(edits[0]).toEqual({ command: "set_active_take", track: 1, group: 10, index: 1 });
    expect(await chooseTake(api, clips[2]!, 2)).toBe(false);
  });
  it("hands the active take to the nearest earlier survivor when it is deleted", () => {
    expect(successors(clips, new Set([12])).map((s) => [s.clip.id, s.index])).toEqual([[11, 1]]);
    expect(successors(clips, new Set([11, 12])).map((s) => s.clip.id)).toEqual([10]);
    expect(successors(clips, new Set([10, 11, 12])).map((s) => s.clip.id)).toEqual([13]);
    expect(successors(clips, new Set([10, 11, 12, 13]))).toEqual([]);
    expect(successors(clips, new Set([11]))).toEqual([]);
  });
  it("deletes the active take by activating another first, in one transaction", async () => {
    const { api, calls, edits } = recorder();
    await deleteClipsKeepingTakes(api, clips, [{ id: 12 }]);
    expect(calls).toEqual(["begin:Delete clip", "set_active_take", "remove_clip", "end"]);
    expect(edits[0]).toMatchObject({ command: "set_active_take", index: 1 });
    expect(edits[1]).toEqual({ command: "remove_clip", id: 12 });
  });
});

describe("stop notices", () => {
  it("prefix a recording's problems", () => {
    expect(stopNotice("Error: 2 loop pass(es) were not recorded", true)).toBe(
      "Recording stopped, with problems: 2 loop pass(es) were not recorded",
    );
    expect(stopNotice("the audio engine is not running", false)).toBe("the audio engine is not running");
  });
});

describe("review fixes", () => {
  it("never creates a range before beat 0", () => {
    const drag = { grab: "create" as const, origin: null, anchor: 3 };
    expect(dragRange(drag, -5, ctx("1"))).toEqual({ start: 0, end: 3 });
    expect(dragRange({ ...drag, anchor: -2 }, -4, ctx("1"))).toBeNull();
  });
  it("hits the bar only on the range, with the grab margin", () => {
    const range = { start: 1, end: 2 };
    expect(onBar(range, 150, 100, 200)).toBe(true);
    expect(onBar(range, 95, 100, 200)).toBe(true);
    expect(onBar(range, 300, 100, 200)).toBe(false);
    expect(onBar(null, 150, 100, 200)).toBe(false);
  });
  it("selection candidates (lane clips) exclude inactive takes", () => {
    const clips = [take(1, 1, 0, false), take(2, 1, 1, true)];
    expect(new Set(laneClips(clips).map((c) => c.id))).toEqual(new Set([2]));
  });
  it("commitRange does nothing when clearing an absent range, and clears punch off", async () => {
    const { api, calls } = recorder();
    expect(await commitRange(api, settings, "punch", null)).toBe(false);
    expect(calls).toEqual([]);
    const set = { ...settings, punch_range: { start: 1, end: 2 }, punch_enabled: true };
    expect(await commitRange(api, set, "punch", null)).toBe(true);
    expect(calls).toEqual(["begin:Set punch range", "set_settings", "end"]);
  });
  it("counts the input device's channels, or none", () => {
    const device = (id: string, inputs: number, def = false) => ({
      id,
      name: id,
      isDefaultOutput: false,
      isDefaultInput: def,
      maxOutputChannels: 2,
      maxInputChannels: inputs,
      sampleRates: [],
      minBufferFrames: null,
      maxBufferFrames: null,
    });
    const lists = (devices: ReturnType<typeof device>[]) => ({
      backends: [{ id: "wasapi", label: "W", devices, error: null }],
      midiInputs: [],
      midiError: null,
      defaultDevice: "default",
    });
    const audio = { backend: "", inputDevice: null };
    expect(inputChannelCount(lists([device("a", 0), device("b", 4, true)]), audio)).toBe(4);
    expect(inputChannelCount(lists([device("a", 0, true)]), audio)).toBeNull();
    expect(inputChannelCount(lists([]), audio)).toBeNull();
    expect(inputChannelCount(lists([device("a", 8), device("b", 2, true)]), { ...audio, inputDevice: "a" })).toBe(8);
    expect(inputChannelCount(lists([device("b", 2, true)]), { ...audio, inputDevice: "gone" })).toBe(2);
  });
});
