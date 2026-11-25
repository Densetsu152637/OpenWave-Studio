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

/** How long a transport position may stand still before the engine counts as stopped. */
export const STALL_MS = 200;
/** After the UI itself starts playback, how long the engine may still look stopped. */
export const START_GRACE_MS = 600;

/**
 * Derives "the engine is moving" from the telemetry position: it advances while the transport
 * plays (a stopped transport holds it), so a position unchanged for `STALL_MS` means stopped.
 * This is how the UI notices a stop at the project end and a play or stop from an agent.
 */
export class MovingTracker {
  #position: number | null = null;
  #changedAt = Number.NEGATIVE_INFINITY;

  /** Feeds a telemetry position seen at `now` (ms). */
  update(position: number | null, now: number): void {
    if (position !== null && position !== this.#position) this.#changedAt = now;
    this.#position = position;
  }

  /** Whether the position moved within the last `STALL_MS`. */
  moving(now: number): boolean {
    return now - this.#changedAt < STALL_MS;
  }
}

/**
 * The UI's `playing` flag given the engine's state. A running engine always shows playing; a
 * stopped one shows stopped, except within `START_GRACE_MS` of the UI's own Play (`startedAt`),
 * so the button does not flicker before the first position change arrives.
 */
export function derivePlaying(moving: boolean, playing: boolean, startedAt: number, now: number): boolean {
  if (moving) return true;
  return playing && now - startedAt < START_GRACE_MS;
}
