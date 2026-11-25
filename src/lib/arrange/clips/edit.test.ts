import { describe, expect, it } from "vitest";
import type { Clip, EditCommand } from "../../ipc/types";
import { SHORTCUT_ACTIONS } from "../../prefs";
import type { ArrangeApi } from "../edits";
import { ARRANGE_SHORTCUTS, arrangeKeyAction } from "../keys";
import { SignatureMap, TempoMap } from "../time";
import { DEFAULT_VIEW } from "../view";
import {
  MAX_GAIN_DB,
  MIN_CLIP_BEATS,
  MIN_GAIN_DB,
  clampFades,
  clampGain,
  commitAdjust,
  dragTarget,
  duplicateClips,
  duplicates,
  setClipsGain,
  setFadeIn,
  setFadeOut,
  splitClip,
  splitClips,
  toggleClipsMuted,
  trimLeft,
  trimRight,
} from "./edit";
import { dropTarget, importAudioFiles, isAudioFile } from "./drop";

const tempo = new TempoMap([{ beat: 0, bpm: 120, ramp: false }]);
const signatures = new SignatureMap([]);
/** 10 seconds of 48 kHz media: 20 beats at 120 BPM, 24000 frames per beat. */
const media = { sample_rate: 48_000, frames: 480_000 };
const BEAT_FRAMES = 24_000;

function audio(extra: Partial<Clip> = {}, content: Partial<Clip["content"]> = {}): Clip {
  return {
    id: 5,
    track: 2,
    name: "loop",
    start: 4,
    length: 4,
    muted: false,
    content: {
      type: "audio",
      media: 7,
      source_offset: 2 * BEAT_FRAMES,
      gain_db: 0,
      fade_in: 0,
      fade_out: 0,
      ...content,
    } as Clip["content"],
    take: null,
    ...extra,
  };
}

function midi(): Clip {
  return {
    id: 9,
    track: 3,
    name: "riff",
    start: 8,
    length: 4,
    muted: false,
    content: {
      type: "midi",
      notes: [
        { start: 0, length: 1, key: 60, velocity: 100 },
        { start: 1.5, length: 1, key: 62, velocity: 90 },
        { start: 2, length: 2, key: 64, velocity: 80 },
      ],
      controllers: [
        { beat: 1, controller: 1, value: 10 },
        { beat: 3, controller: 1, value: 20 },
      ],
    },
    take: null,
  };
}

const offsetOf = (clip: Clip) => (clip.content.type === "audio" ? clip.content.source_offset : -1);

function recorder() {
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
    allocIds: async (count) => {
      calls.push(`alloc:${count}`);
      return Array.from({ length: count }, () => next++);
    },
    mediaImport: async (path) => {
      calls.push(`import:${path}`);
      return { media: 70 + calls.length, frames: 96_000, sampleRate: 48_000, channels: 2, tempo: null, key: null };
    },
  };
  return { api, calls, edits };
}

describe("trim", () => {
  it("left trim moves the start, shortens the clip and advances the source offset", () => {
    const trimmed = trimLeft(audio(), 5, tempo, media);
    expect(trimmed.start).toBe(5);
    expect(trimmed.length).toBe(3);
    // One beat later at 120 BPM is 0.5 s: 24000 media frames.
    expect(offsetOf(trimmed)).toBe(2 * BEAT_FRAMES + BEAT_FRAMES);
    // The audio still ends where it did.
    expect(trimmed.start + trimmed.length).toBe(8);
  });

  it("converts beats to media frames at the media's own rate", () => {
    const slow = { sample_rate: 44_100, frames: 441_000 };
    const trimmed = trimLeft(audio({}, { source_offset: 0 }), 5, tempo, slow);
    expect(offsetOf(trimmed)).toBe(22_050);
  });

  it("uses the tempo at the clip position", () => {
    const fast = new TempoMap([{ beat: 0, bpm: 240, ramp: false }]);
    const trimmed = trimLeft(audio({}, { source_offset: 0 }), 5, fast, media);
    expect(offsetOf(trimmed)).toBe(12_000);
  });

  it("left trim stops where the source offset would go below zero", () => {
    // 2 beats of media sit before the clip's start: the edge cannot go earlier than beat 2.
    const trimmed = trimLeft(audio(), 0, tempo, media);
    expect(trimmed.start).toBe(2);
    expect(offsetOf(trimmed)).toBe(0);
    expect(trimmed.length).toBe(6);
  });

  it("left trim never crosses the right edge and never goes before beat 0", () => {
    const past = trimLeft(audio(), 100, tempo, media);
    expect(past.length).toBeCloseTo(MIN_CLIP_BEATS, 9);
    expect(past.start + past.length).toBe(8);
    const midiBefore = trimLeft(midi(), -5, tempo);
    expect(midiBefore.start).toBe(0);
    expect(midiBefore.length).toBe(12);
  });

  it("right trim changes only the length and stops at the end of the media", () => {
    const shorter = trimRight(audio(), 6, tempo, media);
    expect(shorter.length).toBe(2);
    expect(shorter.start).toBe(4);
    expect(offsetOf(shorter)).toBe(2 * BEAT_FRAMES);
    // 20 beats of media, 2 already skipped: at most 18 beats from the start.
    const longer = trimRight(audio(), 500, tempo, media);
    expect(longer.length).toBeCloseTo(18, 9);
    const framesUsed = offsetOf(longer) + longer.length * BEAT_FRAMES;
    expect(framesUsed).toBeLessThanOrEqual(media.frames + 1e-6);
  });

  it("right trim keeps a positive length", () => {
    expect(trimRight(audio(), -3, tempo, media).length).toBeCloseTo(MIN_CLIP_BEATS, 9);
  });

  it("right-trimming a MIDI clip changes the length only; notes stay", () => {
    const right = trimRight(midi(), 9, tempo);
    expect(right.length).toBe(1);
    expect(right.content).toEqual(midi().content);
  });

  it("left-trimming a MIDI clip keeps notes fixed on the timeline", () => {
    // Edge from beat 8 to 10: the note at 0..1 ends before it and goes, the one at 1.5..2.5
    // straddles it and starts at 0 with length 0.5, the one at 2..4 moves to 0.
    const trimmed = trimLeft(midi(), 10, tempo);
    expect(trimmed.start).toBe(10);
    expect(trimmed.length).toBe(2);
    expect(trimmed.content).toEqual({
      type: "midi",
      notes: [
        { start: 0, length: 0.5, key: 62, velocity: 90 },
        { start: 0, length: 2, key: 64, velocity: 80 },
      ],
      controllers: [{ beat: 1, controller: 1, value: 20 }],
    });
  });

  it("extending a MIDI clip to the left shifts notes later by the same amount", () => {
    const extended = trimLeft(midi(), 6, tempo);
    expect(extended.start).toBe(6);
    expect(extended.length).toBe(6);
    expect(extended.content).toMatchObject({
      notes: [
        { start: 2, length: 1 },
        { start: 3.5, length: 1 },
        { start: 4, length: 2 },
      ],
      controllers: [{ beat: 3 }, { beat: 5 }],
    });
  });

  it("left trim at the start of the media does not extend further", () => {
    const atStart = audio({}, { source_offset: 0 });
    const extended = trimLeft(atStart, 1, tempo, media);
    expect(extended).toEqual(atStart);
    expect(trimLeft(atStart, 5, tempo, media).start).toBe(5);
  });

  it("keeps fades clamped after a trim shortens the clip", () => {
    const faded = audio({}, { fade_in: 2, fade_out: 2 });
    const left = trimLeft(faded, 7, tempo, media);
    expect(left.length).toBe(1);
    expect(left.content).toMatchObject({ fade_in: 1, fade_out: 0 });
  });

  it("shortens fades that no longer fit", () => {
    const faded = audio({}, { fade_in: 1.5, fade_out: 1.5 });
    const trimmed = trimRight(faded, 6, tempo, media);
    expect(trimmed.content).toMatchObject({ fade_in: 1.5, fade_out: 0.5 });
  });

  it("snaps the dragged edge unless Alt is held", () => {
    const view = { ...DEFAULT_VIEW, pxPerBeat: 20 };
    // 1.3 beats of travel from the left edge at beat 4: bar snap (4/4) lands on bar 2 (beat 4).
    expect(dragTarget(audio(), "trim-left", 26, view, signatures, "bar")).toBe(4);
    expect(dragTarget(audio(), "trim-left", 26, view, signatures, "1")).toBe(5);
    expect(dragTarget(audio(), "trim-left", 26, view, signatures, "1", true)).toBeCloseTo(5.3, 9);
    // The right edge starts at beat 8.
    expect(dragTarget(audio(), "trim-right", 40, view, signatures, "1")).toBe(10);
  });

  it("a trim is one ReplaceClip in one transaction, and nothing for no change", async () => {
    const { api, calls, edits } = recorder();
    const trimmed = trimLeft(audio(), 5, tempo, media);
    expect(await commitAdjust(api, audio(), trimmed, "trim-left")).toBe(true);
    expect(calls).toEqual(["begin:Trim clip", "replace_clip", "end"]);
    expect(edits).toEqual([{ command: "replace_clip", clip: trimmed }]);
    expect(await commitAdjust(api, audio(), audio(), "trim-left")).toBe(false);
  });
});

describe("fades", () => {
  it("clamp to the clip and never overlap", () => {
    const clip = audio();
    expect(setFadeIn(clip, 100).content).toMatchObject({ fade_in: 4 });
    expect(setFadeIn(clip, 1).content).toMatchObject({ fade_in: 0 });
    expect(setFadeOut(clip, -100).content).toMatchObject({ fade_out: 4 });
    const withOut = audio({}, { fade_out: 1 });
    expect(setFadeIn(withOut, 100).content).toMatchObject({ fade_in: 3, fade_out: 1 });
    const withIn = audio({}, { fade_in: 1 });
    expect(setFadeOut(withIn, 0).content).toMatchObject({ fade_in: 1, fade_out: 3 });
    expect(setFadeIn(midi(), 9)).toEqual(midi());
  });

  it("clampFades shortens the fade-out first", () => {
    expect(clampFades(audio({}, { fade_in: 3, fade_out: 3 })).content).toMatchObject({
      fade_in: 3,
      fade_out: 1,
    });
  });

  it("dragging a handle reads from where it sits", () => {
    const view = { ...DEFAULT_VIEW, pxPerBeat: 10 };
    const faded = audio({}, { fade_in: 1, fade_out: 2 });
    expect(dragTarget(faded, "fade-in", 10, view, signatures, "off")).toBe(6);
    expect(dragTarget(faded, "fade-out", -10, view, signatures, "off")).toBe(5);
  });
});

describe("gain and mute", () => {
  it("clamps gain to -60..+12 dB", () => {
    expect(clampGain(-100)).toBe(MIN_GAIN_DB);
    expect(clampGain(-Infinity)).toBe(MIN_GAIN_DB);
    expect(clampGain(30)).toBe(MAX_GAIN_DB);
    expect(clampGain(-3.5)).toBe(-3.5);
    expect(clampGain(NaN)).toBe(0);
  });

  it("sets the gain of audio clips in one transaction and skips unchanged and MIDI clips", async () => {
    const { api, calls, edits } = recorder();
    const other = audio({ id: 6 }, { gain_db: 20 });
    expect(await setClipsGain(api, [audio(), other, midi()], 40)).toBe(true);
    expect(calls).toEqual(["begin:Clip gain", "replace_clip", "replace_clip", "end"]);
    expect(edits.map((e) => (e.command === "replace_clip" ? e.clip.content : null))).toEqual([
      expect.objectContaining({ gain_db: 12 }),
      expect.objectContaining({ gain_db: 12 }),
    ]);
    const quiet = recorder();
    expect(await setClipsGain(quiet.api, [audio({}, { gain_db: -3 })], -3)).toBe(false);
    expect(quiet.calls).toEqual([]);
  });

  it("mutes the selection, or unmutes it when all are muted", async () => {
    const first = recorder();
    await toggleClipsMuted(first.api, [audio(), audio({ id: 6, muted: true })]);
    expect(first.calls).toEqual(["begin:Mute clips", "replace_clip", "end"]);
    expect(first.edits[0]).toMatchObject({ clip: { id: 5, muted: true } });
    const second = recorder();
    await toggleClipsMuted(second.api, [audio({ muted: true })]);
    expect(second.calls).toEqual(["begin:Unmute clips", "replace_clip", "end"]);
    expect(second.edits[0]).toMatchObject({ clip: { muted: false } });
  });
});

describe("split", () => {
  it("splits audio and advances the right part's source offset", () => {
    const parts = splitClip(audio({}, { fade_in: 1, fade_out: 2 }), 6, tempo, 50, media)!;
    expect(parts.left).toMatchObject({ id: 5, start: 4, length: 2 });
    expect(parts.right).toMatchObject({ id: 50, start: 6, length: 2 });
    expect(offsetOf(parts.left)).toBe(2 * BEAT_FRAMES);
    expect(offsetOf(parts.right)).toBe(4 * BEAT_FRAMES);
    expect(parts.left.content).toMatchObject({ fade_in: 1, fade_out: 0 });
    expect(parts.right.content).toMatchObject({ fade_in: 0, fade_out: 2 });
  });

  it("returns null outside the clip, at its edges, without media and for patterns", () => {
    expect(splitClip(audio(), 4, tempo, 50, media)).toBeNull();
    expect(splitClip(audio(), 8, tempo, 50, media)).toBeNull();
    expect(splitClip(audio(), 3, tempo, 50, media)).toBeNull();
    expect(splitClip(audio(), 6, tempo, 50)).toBeNull();
    const pattern: Clip = { ...audio(), content: { type: "pattern", pattern: 1 } };
    expect(splitClip(pattern, 6, tempo, 50)).toBeNull();
  });

  it("moves MIDI notes after the split into the right clip with rebased times", () => {
    const parts = splitClip(midi(), 10, tempo, 50)!;
    expect(parts.left).toMatchObject({ id: 9, start: 8, length: 2 });
    expect(parts.right).toMatchObject({ id: 50, start: 10, length: 2 });
    expect(parts.left.content).toEqual({
      type: "midi",
      notes: [
        { start: 0, length: 1, key: 60, velocity: 100 },
        { start: 1.5, length: 1, key: 62, velocity: 90 },
      ],
      controllers: [{ beat: 1, controller: 1, value: 10 }],
    });
    expect(parts.right.content).toEqual({
      type: "midi",
      notes: [{ start: 0, length: 2, key: 64, velocity: 80 }],
      controllers: [{ beat: 1, controller: 1, value: 20 }],
    });
  });

  it("keeps a MIDI note that straddles the split in the left clip, cut at its end", () => {
    // The note at 1.5..2.5 starts before the split at beat 2 of the clip: it stays left.
    const parts = splitClip(midi(), 10, tempo, 50)!;
    expect(parts.left.content).toMatchObject({
      notes: expect.arrayContaining([{ start: 1.5, length: 1, key: 62, velocity: 90 }]),
    });
    expect(parts.right.content).not.toMatchObject({
      notes: expect.arrayContaining([expect.objectContaining({ key: 62 })]),
    });
  });

  it("splits only clips that span the playhead, in one transaction with fresh ids", async () => {
    const { api, calls, edits } = recorder();
    const spans = audio();
    const before = audio({ id: 6, start: 0, length: 2 });
    const ids = await splitClips(api, [spans, before], 6, tempo, () => media);
    expect(ids).toEqual([100]);
    expect(calls).toEqual(["alloc:1", "begin:Split clip", "replace_clip", "add_clip", "end"]);
    expect(edits[0]).toMatchObject({ command: "replace_clip", clip: { id: 5, length: 2 } });
    expect(edits[1]).toMatchObject({ command: "add_clip", clip: { id: 100, start: 6 } });
    const none = recorder();
    expect(await splitClips(none.api, [before], 6, tempo, () => media)).toEqual([]);
    expect(none.calls).toEqual([]);
  });
});

describe("duplicate", () => {
  it("places a copy right after the clip with a fresh id", () => {
    const [copy] = duplicates([audio()], [77]);
    expect(copy).toMatchObject({ id: 77, start: 8, length: 4, track: 2 });
    expect(copy!.content).toEqual(audio().content);
  });

  it("keeps the layout of a group and starts it after the last end", () => {
    const a = audio({ id: 1, start: 0, length: 2 });
    const b = audio({ id: 2, start: 3, length: 4 });
    const copies = duplicates([b, a], [10, 11]);
    expect(copies.map((c) => [c.id, c.start])).toEqual([
      [10, 7],
      [11, 10],
    ]);
  });

  it("copies MIDI notes deeply", () => {
    const [copy] = duplicates([midi()], [3]);
    expect(copy!.content).toEqual(midi().content);
    expect(copy!.content).not.toBe(midi().content);
  });

  it("adds every copy in one transaction", async () => {
    const { api, calls } = recorder();
    const ids = await duplicateClips(api, [audio(), audio({ id: 6 })]);
    expect(ids).toEqual([100, 101]);
    expect(calls).toEqual(["alloc:2", "begin:Duplicate clips", "add_clip", "add_clip", "end"]);
  });
});

describe("drop", () => {
  const geometry = {
    left: 10,
    top: 50,
    width: 1000,
    height: 400,
    scrollTop: 0,
    headerPx: 200,
    topPx: 44,
    lanePx: 88,
  };
  const tracks = [
    { id: 1, kind: "audio" as const },
    { id: 2, kind: "midi" as const },
    { id: 3, kind: "audio" as const },
  ];
  const view = { originBeat: 0, pxPerBeat: 20 };
  const at = (x: number, y: number, snap = "1" as const, bypass = false) =>
    dropTarget(x, y, geometry, view, tracks, signatures, snap, bypass);

  it("maps the point to the track row and the snapped beat", () => {
    // x: 10 (scroller) + 200 (header) + 52 px = beat 2.6 -> 3.
    expect(at(262, 50 + 44 + 10)).toEqual({ trackId: 1, beat: 3 });
    expect(at(262, 50 + 44 + 88 * 2 + 5)).toEqual({ trackId: 3, beat: 3 });
    expect(at(262, 50 + 44 + 10, "1", true)).toEqual({ trackId: 1, beat: 2.6 });
  });

  it("accounts for the scroll position and the view's origin", () => {
    const scrolled = { ...geometry, scrollTop: 88 };
    expect(
      dropTarget(210, 50 + 44 + 10, scrolled, { originBeat: 10, pxPerBeat: 20 }, tracks, signatures, "1"),
    ).toBe("midi");
    expect(
      dropTarget(210 + 40, 50 + 44 + 88 + 10, scrolled, { originBeat: 10, pxPerBeat: 20 }, tracks, signatures, "1"),
    ).toEqual({ trackId: 3, beat: 12 });
  });

  it("refuses MIDI tracks and the ruler, and creates a track below the last one", () => {
    expect(at(262, 50 + 44 + 88 + 10)).toBe("midi");
    expect(at(262, 50 + 10)).toBeNull();
    expect(at(262, 50 + 44 + 88 * 3 + 1)).toEqual({ trackId: null, beat: 3 });
  });

  it("ignores drops over the header column and outside the scroller", () => {
    expect(at(10 + 199, 50 + 44 + 10)).toBeNull();
    expect(at(5, 50 + 44 + 10)).toBeNull();
    expect(at(10 + 1001, 50 + 44 + 10)).toBeNull();
    expect(at(262, 50 + 401)).toBeNull();
    expect(at(262, 20)).toBeNull();
    // Inside the scroller below the last track still makes a new track.
    expect(at(262, 50 + 44 + 88 * 3 + 1)).toEqual({ trackId: null, beat: 3 });
  });

  it("never drops before beat 0", () => {
    expect(at(10 + 200, 50 + 44 + 1)).toEqual({ trackId: 1, beat: 0 });
  });

  it("recognises audio files by extension", () => {
    expect(isAudioFile("C:\\samples\\Kick.WAV")).toBe(true);
    expect(isAudioFile("/a/b/loop.flac")).toBe(true);
    expect(isAudioFile("/a/b/notes.txt")).toBe(false);
    expect(isAudioFile("/a/b/noext")).toBe(false);
  });

  it("stacks several files one after another on the track in one transaction", async () => {
    const { api, calls, edits } = recorder();
    const result = await importAudioFiles(api, tempo, ["/a/one.wav", "/a/skip.txt", "/a/two.mp3"], {
      trackId: 1,
      trackCount: 3,
      start: 4,
    });
    expect(result).toEqual({ clipIds: [100, 101], trackId: 1 });
    expect(calls).toEqual([
      "alloc:2",
      "begin:Import audio files",
      "import:/a/one.wav",
      "add_clip",
      "import:/a/two.mp3",
      "add_clip",
      "end",
    ]);
    // 96000 frames at 48 kHz is 2 s = 4 beats.
    expect(edits[0]).toMatchObject({ clip: { start: 4, length: 4, track: 1, name: "one" } });
    expect(edits[1]).toMatchObject({ clip: { start: 8, length: 4, track: 1, name: "two" } });
  });

  it("creates one new audio track when dropped below the tracks", async () => {
    const { api, edits } = recorder();
    const result = await importAudioFiles(api, tempo, ["/a/one.wav", "/a/two.wav"], {
      trackId: null,
      trackCount: 3,
      start: 0,
    });
    expect(result).toEqual({ clipIds: [100, 101], trackId: 102 });
    expect(edits.map((e) => e.command)).toEqual(["insert_track", "add_clip", "add_clip"]);
    expect(edits[1]).toMatchObject({ clip: { track: 102, start: 0 } });
    expect(edits[2]).toMatchObject({ clip: { track: 102, start: 4 } });
  });

  it("imports nothing when no file is audio", async () => {
    const { api, calls } = recorder();
    expect(await importAudioFiles(api, tempo, ["/a/x.txt"], { trackId: 1, trackCount: 1, start: 0 })).toBeNull();
    expect(calls).toEqual([]);
  });
});

describe("clip editing shortcuts", () => {
  const key = (k: string, extra: Partial<KeyboardEvent> = {}, rollOpen = false) =>
    arrangeKeyAction(
      { key: k, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...extra },
      { rollOpen },
    );

  it("maps S, Ctrl/Cmd+D and 0", () => {
    expect(key("s")).toBe("split");
    expect(key("S", { shiftKey: true })).toBe("split");
    expect(key("d", { ctrlKey: true })).toBe("duplicate");
    expect(key("D", { metaKey: true })).toBe("duplicate");
    expect(key("0")).toBe("toggle-mute");
    expect(key("s", { altKey: true })).toBeNull();
    expect(key("d", { ctrlKey: true, shiftKey: true })).toBeNull();
    expect(key("d")).toBeNull();
  });

  it("ignores auto-repeat for one-shot actions only", () => {
    expect(key("d", { ctrlKey: true, repeat: true })).toBeNull();
    expect(key("s", { repeat: true })).toBeNull();
    expect(key("0", { repeat: true })).toBeNull();
    expect(key("Delete", { repeat: true })).toBeNull();
    expect(key("m", { repeat: true })).toBeNull();
    expect(key("+", { repeat: true })).toBe("zoom-in");
    expect(key("s")).toBe("split");
  });

  it("is off while the piano roll is open", () => {
    expect(key("s", {}, true)).toBeNull();
    expect(key("0", {}, true)).toBeNull();
    expect(key("d", { ctrlKey: true }, true)).toBeNull();
    expect(key("a", { ctrlKey: true }, true)).toBe("select-all");
  });

  it("lists them as fixed shortcuts in settings", () => {
    for (const id of ["arrange.split", "arrange.duplicate", "arrange.toggle-mute"]) {
      expect(ARRANGE_SHORTCUTS.some((s) => s.id === id)).toBe(true);
      expect(SHORTCUT_ACTIONS.find((a) => a.id === id)?.fixed).toBe(true);
    }
  });
});
