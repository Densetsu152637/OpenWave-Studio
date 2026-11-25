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

/** What the UI shows of the transport. */
export interface TransportView {
  playing: boolean;
  recording: boolean;
}

/** The transport state the engine reported in its status. */
export interface EngineTransport {
  moving: boolean;
  countingIn: boolean;
  recording: boolean;
}

/**
 * A status reply issued this soon after a Play, Stop or Record may predate the audio thread
 * applying it, so it is ignored (the next poll settles).
 */
export const SETTLE_MS = 150;

/**
 * The transport view after a status reply. The engine's state wins, so Play re-enables when the
 * engine stops by itself or an agent stops it, and Stop shows when an agent plays. A reply issued
 * (`issuedAt`) before the UI's last own action settled (`actionAt`) is stale and changes nothing.
 * A count-in counts as playing and keeps `recording` as the UI set it, since recording only
 * starts after it.
 */
export function mergeTransport(
  current: TransportView,
  reply: EngineTransport,
  issuedAt: number,
  actionAt: number,
): TransportView {
  if (issuedAt < actionAt + SETTLE_MS) return current;
  const playing = reply.moving || reply.countingIn;
  const recording = reply.recording || (reply.countingIn && current.recording);
  return playing === current.playing && recording === current.recording
    ? current
    : { playing, recording };
}
