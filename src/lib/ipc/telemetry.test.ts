import { describe, expect, it } from "vitest";
import { decodePeaks } from "./peaks";
import { HEADER_WORDS, METER_SLOTS, STRIP_WORDS, decodeFrame, meterSlot } from "./telemetry";

// Produced by `FramePacker::pack` in crates/ow-ipc/tests/telemetry.rs (telemetry_frame_golden_bytes):
// slot 1 subscribed, peaks 0.5/0.25, RMS 0.125/0.0625, position (3 << 24) + 5, load 0.5, ring
// overflow 2, unrouted 1, frame counter 1.
const GOLDEN =
  "0000803f0000803f000040400000a0400000003f000000400000803f000000000000803f0000803f0000003f0000803e0000003e0000803d";

const fromHex = (hex: string): Uint8Array =>
  Uint8Array.from(hex.match(/../g)!.map((byte) => parseInt(byte, 16)));

describe("telemetry frame decoder", () => {
  it("decodes the frame the Rust packer produces", () => {
    const frame = decodeFrame(fromHex(GOLDEN));
    expect(frame).toEqual({
      counter: 1,
      position: 3 * 2 ** 24 + 5,
      cpuLoad: 0.5,
      ringOverflow: 2,
      unrouted: 1,
      midiDropped: 0,
      strips: [{ slot: 1, peak: [0.5, 0.25], rms: [0.125, 0.0625] }],
    });
  });

  it("accepts an ArrayBuffer and a view with an offset", () => {
    const bytes = fromHex(GOLDEN);
    expect(decodeFrame(bytes.buffer as ArrayBuffer)?.strips).toHaveLength(1);
    const padded = new Uint8Array(bytes.length + 8);
    padded.set(bytes, 8);
    expect(decodeFrame(padded.subarray(8))?.counter).toBe(1);
  });

  it("rejects malformed frames", () => {
    const bytes = fromHex(GOLDEN);
    expect(decodeFrame(bytes.subarray(0, bytes.length - 4))).toBeNull();
    expect(decodeFrame(bytes.subarray(0, 7))).toBeNull();
    const wrongVersion = bytes.slice();
    new DataView(wrongVersion.buffer).setFloat32(0, 2, true);
    expect(decodeFrame(wrongVersion)).toBeNull();
    expect(HEADER_WORDS * 4 + STRIP_WORDS * 4).toBe(bytes.length);
  });

  it("handles positions beyond 2^32 frames exactly", () => {
    const bytes = fromHex(GOLDEN);
    const view = new DataView(bytes.buffer);
    view.setFloat32(8, 2 ** 20, true); // high part
    view.setFloat32(12, 0xffffff, true); // low part
    expect(decodeFrame(bytes)?.position).toBe(2 ** 20 * 2 ** 24 + 0xffffff);
  });
});

describe("meter slots", () => {
  const project = { tracks: [{ id: 10 }, { id: 11 }], buses: [{ id: 20 }] };

  it("numbers the master 0, then tracks in order, then buses", () => {
    expect(meterSlot(project, "master")).toBe(0);
    expect(meterSlot(project, { track: 10 })).toBe(1);
    expect(meterSlot(project, { track: 11 })).toBe(2);
    expect(meterSlot(project, { bus: 20 })).toBe(3);
    expect(meterSlot(project, { track: 99 })).toBeNull();
  });

  it("drops strips beyond the bank", () => {
    const many = { tracks: Array.from({ length: METER_SLOTS + 5 }, (_, id) => ({ id })), buses: [] };
    expect(meterSlot(many, { track: METER_SLOTS - 2 })).toBe(METER_SLOTS - 1);
    expect(meterSlot(many, { track: METER_SLOTS - 1 })).toBeNull();
  });
});

describe("peak decoder", () => {
  it("decodes the raw response layout", () => {
    const bytes = new Uint8Array(8 + 16);
    const view = new DataView(bytes.buffer);
    view.setUint32(0, 1, true);
    view.setUint32(4, 2, true);
    [-1, 0.5, -0.25, 0.75].forEach((value, i) => view.setFloat32(8 + 4 * i, value, true));
    const peaks = decodePeaks(bytes);
    expect(peaks?.bins).toBe(2);
    expect([...peaks!.data]).toEqual([-1, 0.5, -0.25, 0.75]);
    expect(decodePeaks(bytes.subarray(0, 20))).toBeNull();
    expect(decodePeaks(new Uint8Array(4))).toBeNull();
  });
});
