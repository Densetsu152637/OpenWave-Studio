import { describe, expect, it } from "vitest";
import type { Clip, EditCommand, Track } from "../ipc/types";
import {
  addTrack,
  clipNameFromPath,
  deleteClip,
  importAudio,
  inTransaction,
  moveClip,
  newAudioTrack,
  renameTrack,
  setTrackStrip,
  validName,
  type ArrangeApi,
} from "./edits";
import { TempoMap } from "./time";

/** Records every call in order; `failOn` makes the named call reject. */
function recorder(failOn?: string) {
  const calls: string[] = [];
  const edits: EditCommand[] = [];
  let next = 100;
  const step = async (name: string) => {
    calls.push(name);
    if (name === failOn) throw new Error(`${name} failed`);
  };
  const api: ArrangeApi = {
    edit: async (edit) => {
      edits.push(edit);
      await step(edit.command);
    },
    begin: async (label) => step(`begin:${label}`),
    end: () => step("end"),
    cancel: () => step("cancel"),
    allocIds: async (count) => {
      await step(`alloc:${count}`);
      return Array.from({ length: count }, () => next++);
    },
    mediaImport: async (path) => {
      await step(`import:${path}`);
      return { media: 7, frames: 96_000, sampleRate: 48_000, channels: 2, tempo: null, key: null };
    },
  };
  return { api, calls, edits };
}

const clip: Clip = {
  id: 5,
  track: 2,
  name: "kick",
  start: 4,
  length: 8,
  muted: false,
  content: { type: "audio", media: 7, source_offset: 480, gain_db: -3, fade_in: 0, fade_out: 0 },
  take: null,
};

const tempo = new TempoMap([{ beat: 0, bpm: 120, ramp: false }]);

describe("clip move", () => {
  it("replaces the clip once, inside one transaction", async () => {
    const { api, calls, edits } = recorder();
    expect(await moveClip(api, clip, 8)).toBe(true);
    expect(calls).toEqual(["begin:Move clip", "replace_clip", "end"]);
    expect(edits).toEqual([{ command: "replace_clip", clip: { ...clip, start: 8 } }]);
  });

  it("sends nothing when the clip stays where it is", async () => {
    const { api, calls } = recorder();
    expect(await moveClip(api, clip, 4)).toBe(false);
    expect(calls).toEqual([]);
  });

  it("cancels the transaction when the edit fails", async () => {
    const { api, calls } = recorder("replace_clip");
    await expect(moveClip(api, clip, 8)).rejects.toThrow("replace_clip failed");
    expect(calls).toEqual(["begin:Move clip", "replace_clip", "cancel"]);
  });
});

describe("import", () => {
  it("imports, then adds the clip at the playhead on the target track, in one transaction", async () => {
    const { api, calls, edits } = recorder();
    const result = await importAudio(api, tempo, "C:\\audio\\Loop One.wav", {
      trackId: 2,
      trackCount: 3,
      start: 4,
    });
    expect(result).toEqual({ clipId: 100, trackId: 2 });
    expect(calls).toEqual([
      "alloc:1",
      "begin:Import audio",
      "import:C:\\audio\\Loop One.wav",
      "add_clip",
      "end",
    ]);
    // 96000 frames at 48 kHz is 2 s, which is 4 beats at 120 bpm.
    expect(edits).toEqual([
      {
        command: "add_clip",
        clip: {
          id: 100,
          track: 2,
          name: "Loop One",
          start: 4,
          length: 4,
          muted: false,
          content: {
            type: "audio",
            media: 7,
            source_offset: 0,
            gain_db: 0,
            fade_in: 0,
            fade_out: 0,
          },
          take: null,
        },
      },
    ]);
  });

  it("makes a new track when none is selected", async () => {
    const { api, calls, edits } = recorder();
    const result = await importAudio(api, tempo, "/a/b.flac", {
      trackId: null,
      trackCount: 1,
      start: 0,
    });
    expect(result).toEqual({ clipId: 100, trackId: 101 });
    expect(calls).toEqual(["alloc:2", "begin:Import audio", "import:/a/b.flac", "insert_track", "add_clip", "end"]);
    expect(edits[0]).toMatchObject({ command: "insert_track", index: 1 });
    expect(edits[1]).toMatchObject({ command: "add_clip", clip: { track: 101, name: "b" } });
  });

  it("cancels when the import fails and adds nothing", async () => {
    const { api, calls } = recorder("import:/x.wav");
    await expect(
      importAudio(api, tempo, "/x.wav", { trackId: 1, trackCount: 1, start: 0 }),
    ).rejects.toThrow("failed");
    expect(calls).toEqual(["alloc:1", "begin:Import audio", "import:/x.wav", "cancel"]);
  });

  it("measures the clip with the tempo map", async () => {
    const { api, edits } = recorder();
    const slow = new TempoMap([{ beat: 0, bpm: 60, ramp: false }]);
    await importAudio(api, slow, "/x.wav", { trackId: 1, trackCount: 1, start: 2 });
    expect(edits[0]).toMatchObject({ clip: { start: 2, length: 2 } });
  });
});

describe("tracks and clips", () => {
  const track: Track = newAudioTrack(2, "Audio 1", 0x4aa3ff).item;

  it("adds an audio track at the end", async () => {
    const { api, calls, edits } = recorder();
    expect(await addTrack(api, 2)).toBe(100);
    expect(calls).toEqual(["alloc:1", "insert_track"]);
    expect(edits[0]).toMatchObject({
      command: "insert_track",
      index: 2,
      bundle: { item: { id: 100, name: "Audio 3", kind: "audio" }, clips: [] },
    });
  });

  it("renames only to an acceptable, different name", async () => {
    const { api, edits } = recorder();
    expect(await renameTrack(api, track, "  Drums ")).toBe(true);
    expect(await renameTrack(api, track, "Audio 1")).toBe(false);
    expect(await renameTrack(api, track, "   ")).toBe(false);
    expect(edits).toEqual([
      { command: "set_info", target: { track: 2 }, name: "Drums", color: 0x4aa3ff },
    ]);
  });

  it("changes mute and volume while keeping the other strip settings", async () => {
    const { api, edits } = recorder();
    await setTrackStrip(api, track, { mute: true });
    await setTrackStrip(api, track, { gain_db: -6 });
    expect(edits).toEqual([
      {
        command: "set_strip_settings",
        target: { track: 2 },
        settings: { ...track.strip.settings, mute: true },
      },
      {
        command: "set_strip_settings",
        target: { track: 2 },
        settings: { ...track.strip.settings, gain_db: -6 },
      },
    ]);
  });

  it("removes a clip", async () => {
    const { api, edits } = recorder();
    await deleteClip(api, clip);
    expect(edits).toEqual([{ command: "remove_clip", id: 5 }]);
  });

  it("validates names like the model", () => {
    expect(validName("Kick")).toBe(true);
    expect(validName("")).toBe(false);
    expect(validName("a\nb")).toBe(false);
    expect(validName("x".repeat(257))).toBe(false);
    expect(validName("x".repeat(256))).toBe(true);
  });

  it("names a clip after its file", () => {
    expect(clipNameFromPath("C:\\a\\b\\take 1.wav")).toBe("take 1");
    expect(clipNameFromPath("/a/b/.hidden")).toBe(".hidden");
    expect(clipNameFromPath("plain")).toBe("plain");
  });
});

describe("transaction helper", () => {
  it("ends after the body and returns its result", async () => {
    const { api, calls } = recorder();
    expect(await inTransaction(api, "x", async () => 42)).toBe(42);
    expect(calls).toEqual(["begin:x", "end"]);
  });
});
