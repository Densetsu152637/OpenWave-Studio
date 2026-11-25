import { describe, expect, it } from "vitest";
import type { Clip } from "../ipc/types";
import { TempoMap } from "./time";
import { MIN_BIN_FRAMES, peakRequest, waveformColumns } from "./waveform";

const tempo = new TempoMap([{ beat: 0, bpm: 120, ramp: false }]);
const media = { sample_rate: 48_000, frames: 480_000 };

function audioClip(over: Partial<Clip> = {}, offset = 0): Clip {
  return {
    id: 1,
    track: 1,
    name: "c",
    start: 4,
    length: 8,
    muted: false,
    content: { type: "audio", media: 9, source_offset: offset, gain_db: 0, fade_in: 0, fade_out: 0 },
    take: null,
    ...over,
  };
}

describe("peak request", () => {
  it("asks one bin per pixel over the clip's frames", () => {
    // 8 beats at 120 bpm is 4 s = 192000 frames over 400 px.
    expect(peakRequest(audioClip(), media, tempo, 400, 0, 400)).toEqual({
      media: 9,
      start: 0,
      end: 192_000,
      bins: 400,
    });
  });

  it("maps a visible part of the clip to its frames", () => {
    expect(peakRequest(audioClip(), media, tempo, 400, 100, 200)).toEqual({
      media: 9,
      start: 48_000,
      end: 96_000,
      bins: 100,
    });
  });

  it("starts at the source offset and stops at the end of the media", () => {
    const request = peakRequest(audioClip({}, 400_000), media, tempo, 400, 0, 400)!;
    expect(request.start).toBe(400_000);
    expect(request.end).toBe(480_000);
  });

  it("never asks for bins narrower than 64 frames", () => {
    // 4 s over 100000 px would be under 2 frames a pixel.
    const request = peakRequest(audioClip(), media, tempo, 100_000, 0, 50_000)!;
    expect((request.end - request.start) / request.bins).toBeGreaterThanOrEqual(MIN_BIN_FRAMES);
    expect(request.bins).toBe(Math.floor((request.end - request.start) / MIN_BIN_FRAMES));
    // A span shorter than one bin still gets one.
    expect(peakRequest(audioClip({ length: 0.0005 }), media, tempo, 2, 0, 2)!.bins).toBe(1);
  });

  it("has nothing to draw for MIDI clips or empty spans", () => {
    const midi = audioClip({ content: { type: "midi", notes: [], controllers: [] } });
    expect(peakRequest(midi, media, tempo, 400, 0, 400)).toBeNull();
    expect(peakRequest(audioClip(), media, tempo, 400, 50, 50)).toBeNull();
    expect(peakRequest(audioClip({}, 480_000), media, tempo, 400, 0, 400)).toBeNull();
  });
});

describe("waveform columns", () => {
  it("takes the extremes of the bins a column covers", () => {
    const data = new Float32Array([-0.1, 0.2, -0.5, 0.1, -0.2, 0.9, -0.3, 0.3]);
    expect(Array.from(waveformColumns(data, 2))).toEqual([
      expect.closeTo(-0.5),
      expect.closeTo(0.2),
      expect.closeTo(-0.3),
      expect.closeTo(0.9),
    ]);
  });

  it("repeats bins when there are more columns than bins", () => {
    const data = new Float32Array([-1, 1, -0.5, 0.5]);
    expect(Array.from(waveformColumns(data, 4))).toEqual([-1, 1, -1, 1, -0.5, 0.5, -0.5, 0.5]);
  });

  it("draws silence for no data", () => {
    expect(Array.from(waveformColumns(new Float32Array(0), 2))).toEqual([0, 0, 0, 0]);
  });
});
