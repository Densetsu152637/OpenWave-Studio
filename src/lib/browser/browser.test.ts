import { describe, expect, it } from "vitest";
import type { ArrangeApi } from "../arrange/edits";
import { TempoMap } from "../arrange/time";
import type { Clip, EditCommand, LibraryScanStatus, MediaImportReply, MediaItem } from "../ipc/types";
import { addLibraryEntry } from "./add";
import {
  KEY_CHOICES,
  NO_FILTERS,
  autoWarp,
  bpmLabel,
  durationLabel,
  emptyHint,
  keyLabel,
  loopBeats,
  parseKeyChoice,
  scanFraction,
  scanSummary,
  toQuery,
} from "./library";
import {
  clampTranspose,
  setClipsTranspose,
  setClipsWarp,
  sharedSourceBpm,
  sharedTranspose,
  warpState,
  withWarp,
} from "./warp";

const tempo = (bpm: number) => new TempoMap([{ beat: 0, bpm, ramp: false }]);

function recorder(reply: Partial<MediaImportReply> = {}) {
  const calls: string[] = [];
  const edits: EditCommand[] = [];
  let next = 100;
  const api: ArrangeApi = {
    edit: async (edit) => {
      edits.push(edit);
      calls.push(edit.command);
    },
    begin: async (label) => void calls.push(`begin:${label}`),
    end: async () => void calls.push("end"),
    cancel: async () => void calls.push("cancel"),
    allocIds: async (count) => Array.from({ length: count }, () => next++),
    mediaImport: async () => ({
      media: 7,
      frames: 384_000,
      sampleRate: 48_000,
      channels: 2,
      tempo: null,
      key: null,
      ...reply,
    }),
  };
  return { api, calls, edits };
}

describe("labels and filters", () => {
  it("names keys and parses the key filter", () => {
    expect(keyLabel({ tonic: 9, minor: true })).toBe("Am");
    expect(keyLabel({ tonic: 6, minor: false })).toBe("F#");
    expect(keyLabel(null)).toBe("");
    expect(KEY_CHOICES).toHaveLength(25);
    expect(parseKeyChoice("9m")).toEqual({ tonic: 9, minor: true });
    expect(parseKeyChoice("0M")).toEqual({ tonic: 0, minor: false });
    expect(parseKeyChoice("")).toBeNull();
    expect(parseKeyChoice("12m")).toBeNull();
    for (const choice of KEY_CHOICES.slice(1)) {
      expect(keyLabel(parseKeyChoice(choice.value))).toBe(choice.label);
    }
  });

  it("turns the filter row into a query, ignoring empty and invalid tempi", () => {
    expect(toQuery(NO_FILTERS)).toMatchObject({
      text: "",
      bpmMin: null,
      bpmMax: null,
      key: null,
      includeHalfDouble: false,
    });
    expect(
      toQuery({ text: " kick ", bpmMin: "90", bpmMax: "x", key: "9m", halfDouble: true }),
    ).toMatchObject({
      text: "kick",
      bpmMin: 90,
      bpmMax: null,
      key: { tonic: 9, minor: true },
      includeHalfDouble: true,
    });
    expect(toQuery({ ...NO_FILTERS, bpmMin: "-5" }).bpmMin).toBeNull();
  });

  it("formats tempo and length", () => {
    expect(bpmLabel(120)).toBe("120");
    expect(bpmLabel(87.54)).toBe("87.5");
    expect(bpmLabel(null)).toBe("–");
    expect(durationLabel(8)).toBe("0:08");
    expect(durationLabel(125.4)).toBe("2:05");
    expect(durationLabel(0)).toBe("–");
  });

  it("reports scan progress and explains an empty list", () => {
    const status: LibraryScanStatus = {
      scanning: true,
      done: 3,
      total: 12,
      entries: 0,
      analysed: 0,
      cached: 0,
      failed: 0,
      cancelled: false,
      roots: [],
    };
    expect(scanFraction(status)).toBe(0.25);
    expect(scanFraction({ ...status, total: 0 })).toBeNull();
    expect(scanFraction({ ...status, scanning: false })).toBeNull();
    expect(scanSummary(status)).toBe("Scanning 3 of 12 files…");
    expect(scanSummary({ ...status, scanning: false, entries: 1, failed: 1, cancelled: true })).toBe(
      "1 file, 1 could not be read (scan stopped)",
    );
    expect(emptyHint(0, false, 0)).toContain("Settings");
    expect(emptyHint(0, false, 2)).toContain("Scan");
    expect(emptyHint(5, false, 2)).toBe("No file matches.");
    expect(emptyHint(0, true, 2)).toContain("Scanning");
  });
});

describe("automatic warp of a loop", () => {
  it("takes a nearly whole number of beats as whole, so the loop fills its bars", () => {
    const loop = loopBeats(8.0, 119.98);
    expect(loop.beats).toBe(16);
    expect((loop.beats * 60) / 8).toBeCloseTo(loop.bpm, 9);
    expect(loopBeats(8.0, 100)).toEqual({ beats: (8 * 100) / 60, bpm: 100 });
  });

  it("warps loops of up to 32 bars with a known tempo and nothing else", () => {
    expect(autoWarp(120, 8)).toEqual({
      mode: "follow_tempo",
      source_bpm: 120,
      markers: [],
      transpose_semitones: 0,
    });
    // 32 bars at 120 bpm is 64 seconds; one more beat is too long.
    expect(autoWarp(120, 64)).not.toBeNull();
    expect(autoWarp(120, 64.6)).toBeNull();
    expect(autoWarp(null, 8)).toBeNull();
    expect(autoWarp(120, 0)).toBeNull();
    expect(autoWarp(0, 8)).toBeNull();
    expect(autoWarp(120, 0.2)).toBeNull();
  });
});

describe("adding a library file", () => {
  it("imports it and adds a warped clip on a new track in one undo step", async () => {
    const { api, calls, edits } = recorder();
    const result = await addLibraryEntry(
      api,
      tempo(90),
      { path: "/lib/Drums/loop_120.wav", name: "loop_120.wav", bpm: 120 },
      { trackId: null, trackCount: 2, start: 4 },
    );
    expect(result).toEqual({ clipId: 100, trackId: 101, warped: true });
    expect(calls).toEqual(["begin:Add loop", "insert_track", "add_clip", "end"]);
    const add = edits.find((e) => e.command === "add_clip");
    expect(add?.command === "add_clip" && add.clip).toMatchObject({
      name: "loop_120",
      start: 4,
      // 8 seconds of audio at 120 bpm is 16 beats, whatever the project tempo.
      length: 16,
      content: {
        type: "audio",
        warp: { mode: "follow_tempo", source_bpm: 120, markers: [], transpose_semitones: 0 },
      },
    });
  });

  it("uses the tempo detected on import when the file name has none, and leaves tempo-less files alone", async () => {
    const detected = recorder({ tempo: 100 });
    await addLibraryEntry(
      detected.api,
      tempo(120),
      { path: "/lib/a.wav", name: "a.wav", bpm: null },
      { trackId: 5, trackCount: 3, start: 0 },
    );
    const clip = (detected.edits.at(-1) as Extract<EditCommand, { command: "add_clip" }>).clip;
    expect(clip.track).toBe(5);
    expect(clip.content).toMatchObject({ warp: { source_bpm: expect.closeTo(100, 6) } });

    const plain = recorder();
    const result = await addLibraryEntry(
      plain.api,
      tempo(120),
      { path: "/lib/kick.wav", name: "kick.wav", bpm: null },
      { trackId: 5, trackCount: 3, start: 0 },
    );
    expect(result.warped).toBe(false);
    const kick = (plain.edits.at(-1) as Extract<EditCommand, { command: "add_clip" }>).clip;
    expect(kick.content).not.toHaveProperty("warp");
    expect(kick.length).toBeCloseTo(8 * 2, 9); // 8 s at 120 bpm
  });

  it("cancels the transaction when the import fails", async () => {
    const { api, calls } = recorder();
    api.mediaImport = async () => {
      throw new Error("decode failed");
    };
    await expect(
      addLibraryEntry(api, tempo(120), { path: "/x.wav", name: "x.wav", bpm: 100 }, { trackId: 1, trackCount: 1, start: 0 }),
    ).rejects.toThrow("decode failed");
    expect(calls).toEqual(["begin:Add loop", "cancel"]);
  });
});

const media: MediaItem = {
  id: 7,
  name: "loop",
  path: "audio/x.wav",
  hash: "x",
  sample_rate: 48_000,
  channels: 2,
  frames: 384_000,
  tempo: 120,
};

const audioClip = (over: Partial<Clip> = {}): Clip => ({
  id: 5,
  track: 2,
  name: "loop",
  start: 4,
  length: 16,
  muted: false,
  content: { type: "audio", media: 7, source_offset: 0, gain_db: 0, fade_in: 0, fade_out: 0 },
  take: null,
  ...over,
});

const warped = (transpose = 0, bpm = 120): Clip => ({
  ...audioClip(),
  content: {
    type: "audio",
    media: 7,
    source_offset: 0,
    gain_db: 0,
    fade_in: 0,
    fade_out: 0,
    warp: { mode: "follow_tempo", source_bpm: bpm, markers: [], transpose_semitones: transpose },
  },
});

describe("warp on selected clips", () => {
  it("summarises the selection", () => {
    expect(warpState([])).toBeNull();
    expect(warpState([audioClip()])).toBe("off");
    expect(warpState([warped()])).toBe("on");
    expect(warpState([audioClip(), warped()])).toBe("mixed");
    expect(sharedSourceBpm([warped(0, 120), warped(0, 120)])).toBe(120);
    expect(sharedSourceBpm([warped(0, 120), warped(0, 90)])).toBeNull();
    expect(sharedSourceBpm([audioClip()])).toBeNull();
    expect(sharedTranspose([warped(2), warped(2)])).toBe(2);
    expect(sharedTranspose([warped(2), warped(3)])).toBeNull();
    expect(clampTranspose(40)).toBe(24);
    expect(clampTranspose(-2.456)).toBe(-2.46);
    expect(clampTranspose(Number.NaN)).toBe(0);
  });

  it("turning warp on keeps the audio the clip plays (its length follows the source tempo)", () => {
    // 16 beats at 120 bpm project tempo is 8 s; at a source tempo of 100 that is 13.33 beats.
    const next = withWarp(audioClip(), true, { ...media, tempo: 100 }, tempo(120));
    expect(next.content).toMatchObject({
      warp: { mode: "follow_tempo", source_bpm: 100, transpose_semitones: 0 },
    });
    expect(next.length).toBeCloseTo((8 * 100) / 60, 9);
  });

  it("turning warp off plays the same audio again at the project tempo", () => {
    const clip = warped(3, 100);
    const off = withWarp({ ...clip, length: 20 }, false, media, tempo(90));
    // 20 beats at 100 bpm is 12 s, which is 18 beats at 90 bpm.
    expect(off.length).toBeCloseTo(18, 9);
    expect(off.content).toMatchObject({ warp: { mode: "off", transpose_semitones: 0 } });
  });

  it("refuses to warp audio with no known tempo", () => {
    expect(() => withWarp(audioClip(), true, { ...media, tempo: null }, tempo(120))).toThrow(
      /No tempo/,
    );
  });

  it("changes every selected audio clip in one transaction", async () => {
    const { api, calls, edits } = recorder();
    const changed = await setClipsWarp(api, [audioClip(), audioClip({ id: 6 })], true, () => media, tempo(120));
    expect(changed).toBe(true);
    expect(calls).toEqual(["begin:Warp clips", "replace_clip", "replace_clip", "end"]);
    expect(edits).toHaveLength(2);
    // Nothing to do when they already agree.
    const again = recorder();
    expect(await setClipsWarp(again.api, [warped()], true, () => media, tempo(120))).toBe(false);
    expect(again.calls).toEqual([]);
  });

  it("transposes only the warped clips, one undo step", async () => {
    const { api, calls, edits } = recorder();
    expect(await setClipsTranspose(api, [warped(), audioClip({ id: 9 })], 99)).toBe(true);
    expect(calls).toEqual(["begin:Transpose clips", "replace_clip", "end"]);
    const clip = (edits[0] as Extract<EditCommand, { command: "replace_clip" }>).clip;
    expect(clip.content).toMatchObject({ warp: { transpose_semitones: 24 } });
    const same = recorder();
    expect(await setClipsTranspose(same.api, [warped(24)], 24)).toBe(false);
  });
});

describe("editing a warped clip", () => {
  it("splits and trims at the source tempo, not the project tempo", async () => {
    const { splitClip, trimLeft, trimRight } = await import("../arrange/clips/edit");
    const bounds = { sample_rate: 48_000, frames: 384_000 };
    // 8 s loop at 120 bpm = 16 beats; the project plays at 90 bpm.
    const clip = warped(0, 120);
    const split = splitClip(clip, 12, tempo(90), 99, bounds);
    // The left part covers 8 beats of source: 4 s at 120 bpm.
    expect(split?.right.content).toMatchObject({ source_offset: 4 * 48_000 });
    const left = trimLeft(clip, 8, tempo(90), bounds);
    expect(left.content).toMatchObject({ source_offset: 2 * 48_000 });
    // The right edge stops where the source ends: 16 beats from the start.
    const right = trimRight({ ...clip, length: 4 }, 100, tempo(90), bounds);
    expect(right.length).toBeCloseTo(16, 9);
  });
});

describe("warp markers", () => {
  const withMarkers = (): Clip => ({
    ...warped(0, 120),
    content: {
      type: "audio",
      media: 7,
      source_offset: 0,
      gain_db: 0,
      fade_in: 0,
      fade_out: 0,
      warp: {
        mode: "follow_tempo",
        source_bpm: 120,
        markers: [
          { source_frame: 24_000, beat: 1 },
          { source_frame: 120_000, beat: 4 },
        ],
        transpose_semitones: 0,
      },
    },
  });
  const bounds = { sample_rate: 48_000, frames: 384_000 };

  it("a left trim keeps every later beat on the same source frame", async () => {
    const { trimLeft } = await import("../arrange/clips/edit");
    const { sourceFrameAt } = await import("./markers");
    const clip = withMarkers();
    const trimmed = trimLeft(clip, clip.start + 2, tempo(90), bounds);
    if (clip.content.type !== "audio" || trimmed.content.type !== "audio") throw new Error("audio");
    const before = clip.content.warp!;
    const after = trimmed.content.warp!;
    expect(after.markers[0]).toEqual({ source_frame: expect.closeTo(56_000, 0), beat: 0 });
    expect(after.markers[1]).toEqual({ source_frame: 120_000, beat: 2 });
    for (const beat of [0, 1, 2, 3, 5.5]) {
      expect(sourceFrameAt(after, beat, 48_000, 0)).toBeCloseTo(
        sourceFrameAt(before, beat + 2, 48_000, 0),
        0,
      );
    }
  });

  it("a split gives the right part the shifted markers and leaves the left part alone", async () => {
    const { splitClip } = await import("../arrange/clips/edit");
    const { sourceFrameAt } = await import("./markers");
    const clip = withMarkers();
    const parts = splitClip(clip, clip.start + 6, tempo(90), 99, bounds);
    if (!parts || parts.left.content.type !== "audio" || parts.right.content.type !== "audio") {
      throw new Error("split");
    }
    expect(parts.left.content.warp?.markers).toEqual(clip.content.type === "audio" ? clip.content.warp?.markers : []);
    const right = parts.right.content.warp!;
    // Beat 6 of the original is beat 0 of the right part.
    expect(sourceFrameAt(right, 0, 48_000, 0)).toBeCloseTo(
      sourceFrameAt(parts.left.content.warp!, 6, 48_000, 0),
      0,
    );
    expect(right.markers).toHaveLength(1);
    expect(right.markers[0]!.beat).toBe(0);
  });

  it("flags clips whose stretch is beyond 4x", async () => {
    const { warpOutOfRange } = await import("./markers");
    const clip = warped(0, 120);
    expect(warpOutOfRange(clip, tempo(90), 48_000)).toBe(false);
    expect(warpOutOfRange(clip, tempo(30), 48_000)).toBe(false); // exactly 4x
    expect(warpOutOfRange(clip, tempo(25), 48_000)).toBe(true);
    expect(warpOutOfRange(clip, tempo(600), 48_000)).toBe(true);
    expect(warpOutOfRange(audioClip(), tempo(25), 48_000)).toBe(false);
  });

  it("flags a short stretch beyond 4x between tempo changes", async () => {
    const { warpOutOfRange } = await import("./markers");
    const clip = warped(0, 120); // beats 4 to 20
    const dip = new TempoMap([
      { beat: 0, bpm: 120, ramp: false },
      { beat: 10, bpm: 20, ramp: false }, // 6x slower for half a beat
      { beat: 10.5, bpm: 120, ramp: false },
    ]);
    expect(warpOutOfRange(clip, dip, 48_000)).toBe(true);
    const mild = new TempoMap([
      { beat: 0, bpm: 120, ramp: false },
      { beat: 10, bpm: 90, ramp: false },
      { beat: 10.5, bpm: 120, ramp: false },
    ]);
    expect(warpOutOfRange(clip, mild, 48_000)).toBe(false);
  });
});
