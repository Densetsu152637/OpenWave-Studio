import { describe, expect, it } from "vitest";
import { SETTLE_MS, mergeTransport, type EngineTransport } from "./playback";

const idle: EngineTransport = { moving: false, countingIn: false, recording: false };
const stopped = { playing: false, recording: false };
const playing = { playing: true, recording: false };

describe("transport view merge", () => {
  it("ignores a status reply issued before a Stop settled", () => {
    // Stop pressed at t=1000 (optimistic false); a reply sent at 1000 still says moving.
    const reply = { ...idle, moving: true };
    expect(mergeTransport(stopped, reply, 1000, 1000)).toBe(stopped);
    expect(mergeTransport(stopped, reply, 1000 + SETTLE_MS - 1, 1000)).toBe(stopped);
    expect(mergeTransport(stopped, idle, 1000 + SETTLE_MS, 1000)).toBe(stopped);
  });

  it("keeps an optimistic Play until a reply issued after it settled", () => {
    expect(mergeTransport(playing, idle, 1000, 1000)).toBe(playing);
    expect(mergeTransport(playing, { ...idle, moving: true }, 1200, 1000)).toBe(playing);
  });

  it("a locate while stopped does not show playing", () => {
    expect(mergeTransport(stopped, idle, 5000, 0)).toBe(stopped);
  });

  it("a count-in shows playing and keeps recording", () => {
    const view = { playing: true, recording: true };
    expect(mergeTransport(view, { ...idle, countingIn: true }, 5000, 1000)).toBe(view);
    expect(
      mergeTransport(view, { moving: true, countingIn: false, recording: true }, 6000, 1000),
    ).toBe(view);
  });

  it("an engine that stops itself re-enables Play and clears recording", () => {
    expect(mergeTransport(playing, idle, 9000, 1000)).toEqual(stopped);
    const recording = { playing: true, recording: true };
    expect(mergeTransport(recording, idle, 9000, 1000)).toEqual(stopped);
  });

  it("an agent starting or stopping playback shows in the view", () => {
    expect(mergeTransport(stopped, { ...idle, moving: true }, 9000, 1000)).toEqual(playing);
    expect(
      mergeTransport(stopped, { moving: true, countingIn: false, recording: true }, 9000, 1000),
    ).toEqual({ playing: true, recording: true });
    expect(mergeTransport(playing, idle, 9000, 1000)).toEqual(stopped);
  });
});
