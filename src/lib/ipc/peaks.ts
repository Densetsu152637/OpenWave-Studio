import type { IpcTransport } from "./transport";
import { tauriTransport } from "./transport";

/**
 * Waveform min/max pairs. The `peaks_range` command answers with raw bytes (a `tauri::ipc::Response`),
 * layout in `crates/ow-ipc/src/peaks.rs`: `u32` version (1), `u32` bin count `n`, then `n` pairs of
 * little-endian `f32` (minimum, maximum). The source is a stub until the stage 11 peak cache.
 */
export const PEAKS_VERSION = 1;

export interface PeakBins {
  /** Interleaved minimum, maximum per bin: `[min0, max0, min1, max1, ...]`. */
  data: Float32Array;
  bins: number;
}

/** Decodes a peak response; null when malformed. */
export function decodePeaks(bytes: ArrayBuffer | ArrayBufferView): PeakBins | null {
  const view = ArrayBuffer.isView(bytes)
    ? new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
    : new DataView(bytes);
  if (view.byteLength < 8 || view.getUint32(0, true) !== PEAKS_VERSION) return null;
  const bins = view.getUint32(4, true);
  if (view.byteLength !== 8 + 8 * bins) return null;
  // Copy out with explicit little-endian reads: the buffer may be unaligned or big-endian host.
  const data = new Float32Array(bins * 2);
  for (let i = 0; i < data.length; i++) data[i] = view.getFloat32(8 + 4 * i, true);
  return { data, bins };
}

/** Asks for `bins` min/max pairs over frames `[start, end)` of a media item. */
export async function peaksRange(
  media: number,
  start: number,
  end: number,
  bins: number,
  transport: IpcTransport = tauriTransport,
): Promise<PeakBins> {
  const bytes = await transport.invoke<ArrayBuffer | number[]>("peaks_range", { media, start, end, bins });
  const buffer = Array.isArray(bytes) ? new Uint8Array(bytes).buffer : bytes;
  const decoded = decodePeaks(buffer);
  if (!decoded) throw new Error("malformed peak response");
  return decoded;
}
