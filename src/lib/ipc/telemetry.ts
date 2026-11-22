/**
 * Decoder for the high-rate telemetry frames the UI bridge thread sends over a Tauri
 * `ipc::Channel` as raw bytes. Mirrors the frame layout documented in `crates/ow-ipc/src/telemetry.rs`
 * (version 1): little-endian `f32` words, `9 + 5 * n` of them for `n` subscribed strips.
 *
 * | word | content |
 * | --- | --- |
 * | 0 | format version, 1.0 |
 * | 1 | frame counter, wraps at 2^24 |
 * | 2, 3 | transport position in frames: high part (`position >> 24`) and low part (`position & 0xFFFFFF`) |
 * | 4 | callback CPU load, 1.0 is the whole buffer period |
 * | 5, 6, 7 | dropped control messages, unrouted control messages, dropped MIDI events (saturate at 2^24 - 1) |
 * | 8 | `n` |
 * | `9 + 5 i ..` | strip `i`: meter slot, peak left, peak right, RMS left, RMS right |
 */
import type { MixerRef } from "./types";

export const FRAME_VERSION = 1;
export const HEADER_WORDS = 9;
export const STRIP_WORDS = 5;
/** Meter slots in the engine's telemetry bank. */
export const METER_SLOTS = 256;
export const MASTER_SLOT = 0;
const WORD_BYTES = 4;
const LOW_BITS = 0x1000000; // 2^24

export interface StripMeter {
  slot: number;
  /** Peak per channel (linear) since the previous frame. */
  peak: [number, number];
  /** RMS per channel (linear) of the latest block. */
  rms: [number, number];
}

export interface TelemetryFrame {
  /** Wraps at 2^24; a jump of more than one means frames were dropped. */
  counter: number;
  /** Transport position in sample frames. */
  position: number;
  /** Audio callback load, 0 idle to 1 the whole buffer period. */
  cpuLoad: number;
  ringOverflow: number;
  unrouted: number;
  midiDropped: number;
  strips: StripMeter[];
}

/** Decodes one frame; null when the bytes are not a well-formed version-1 frame. */
export function decodeFrame(bytes: ArrayBuffer | ArrayBufferView): TelemetryFrame | null {
  const view = ArrayBuffer.isView(bytes)
    ? new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
    : new DataView(bytes);
  if (view.byteLength % WORD_BYTES !== 0 || view.byteLength < HEADER_WORDS * WORD_BYTES) return null;
  const word = (i: number) => view.getFloat32(i * WORD_BYTES, true);
  if (word(0) !== FRAME_VERSION) return null;
  const count = word(8);
  if (!Number.isInteger(count) || count < 0) return null;
  if (view.byteLength !== (HEADER_WORDS + STRIP_WORDS * count) * WORD_BYTES) return null;
  const strips: StripMeter[] = [];
  for (let i = 0; i < count; i++) {
    const at = HEADER_WORDS + STRIP_WORDS * i;
    strips.push({
      slot: word(at),
      peak: [word(at + 1), word(at + 2)],
      rms: [word(at + 3), word(at + 4)],
    });
  }
  return {
    counter: word(1),
    position: word(2) * LOW_BITS + word(3),
    cpuLoad: word(4),
    ringOverflow: word(5),
    unrouted: word(6),
    midiDropped: word(7),
    strips,
  };
}

/** The part of the mirrored project meter slots derive from. */
export interface SlotSource {
  tracks: readonly { id: number }[];
  buses: readonly { id: number }[];
}

/**
 * The meter slot of a strip: the master is slot 0, then tracks in display order, then buses.
 * Null when the strip does not exist or falls beyond the bank. The same rule as `meter_slot` in
 * `ow-ipc`, so the UI can subscribe to strips from its mirror alone.
 */
export function meterSlot(project: SlotSource, strip: MixerRef): number | null {
  let slot: number;
  if (strip === "master") {
    slot = MASTER_SLOT;
  } else if ("track" in strip) {
    const index = project.tracks.findIndex((t) => t.id === strip.track);
    if (index < 0) return null;
    slot = 1 + index;
  } else {
    const index = project.buses.findIndex((b) => b.id === strip.bus);
    if (index < 0) return null;
    slot = 1 + project.tracks.length + index;
  }
  return slot < METER_SLOTS ? slot : null;
}
