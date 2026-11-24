import { tauriTransport, type IpcTransport } from "../ipc/transport";
import type { SignatureMap } from "./time";

/** Starts playback from the current position. */
export function transportPlay(transport: IpcTransport = tauriTransport): Promise<void> {
  return transport.invoke<void>("transport_play");
}

/** Stops playback; the position holds. */
export function transportStop(transport: IpcTransport = tauriTransport): Promise<void> {
  return transport.invoke<void>("transport_stop");
}

/** Moves the transport to `beats` (also while playing). */
export function transportLocate(
  beats: number,
  transport: IpcTransport = tauriTransport,
): Promise<void> {
  return transport.invoke<void>("transport_locate", { beats });
}

/** "bar.beat" (both 1-based) for the transport readout. */
export function positionLabel(beat: number, signatures: SignatureMap): string {
  const bar = signatures.barAt(beat);
  const beatLength = bar.length / bar.beats;
  const inBar = Math.min(bar.beats - 1, Math.max(0, Math.floor((beat - bar.start) / beatLength + 1e-9)));
  return `${bar.index + 1}.${inBar + 1}`;
}
