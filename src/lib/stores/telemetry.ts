import { decodeFrame, type TelemetryFrame } from "../ipc/telemetry";
import { tauriTransport, type IpcTransport } from "../ipc/transport";

type Subscriber = (frame: TelemetryFrame | null) => void;

/** Frames per second asked of the bridge thread (it caps at 60). */
export const TELEMETRY_HZ = 60;

/**
 * The latest telemetry frame as a read-only Svelte store (`$telemetry`): meters for the subscribed
 * strips, transport position, CPU load and overflow counters. Frames arrive as raw bytes from the
 * UI bridge thread; only strips passed to `setVisibleStrips` are included, so views report what
 * they currently show (by meter slot, see `meterSlot` in `$lib/ipc/telemetry`).
 */
export class TelemetryStore {
  readonly #transport: IpcTransport;
  readonly #subscribers = new Set<Subscriber>();
  #frame: TelemetryFrame | null = null;
  #running = false;
  #starting: Promise<void> | null = null;
  #dropped = 0;

  constructor(transport: IpcTransport = tauriTransport) {
    this.#transport = transport;
  }

  get frame(): TelemetryFrame | null {
    return this.#frame;
  }

  /** Frames whose counter skipped ahead (dropped on the way). */
  get droppedFrames(): number {
    return this.#dropped;
  }

  subscribe(run: Subscriber): () => void {
    this.#subscribers.add(run);
    run(this.#frame);
    return () => {
      this.#subscribers.delete(run);
    };
  }

  /** Starts the bridge thread streaming into this store. */
  async start(hz: number = TELEMETRY_HZ): Promise<void> {
    if (!this.#transport.available || this.#running) return;
    this.#running = true;
    const starting = this.#transport
      .invoke<void>("telemetry_start", {
        frames: this.#transport.channel<ArrayBuffer | number[]>((raw) => this.receive(raw)),
        hz,
      })
      .catch((error) => {
        this.#running = false;
        throw error;
      });
    this.#starting = starting.then(
      () => undefined,
      () => undefined,
    );
    await starting;
  }

  async stop(): Promise<void> {
    if (!this.#transport.available) return;
    // A start still in flight must reach the backend first, or its bridge would outlive this stop.
    await this.#starting;
    if (!this.#running) return;
    this.#running = false;
    await this.#transport.invoke<void>("telemetry_stop");
  }

  /** Reports which meter slots are on screen. */
  async setVisibleStrips(slots: readonly number[]): Promise<void> {
    if (!this.#transport.available) return;
    await this.#transport.invoke<void>("telemetry_set_strips", { slots: [...slots] });
  }

  /** Entry point for the frame channel (also used by tests). */
  receive(raw: ArrayBuffer | ArrayBufferView | number[]): void {
    const bytes = Array.isArray(raw) ? new Uint8Array(raw) : raw;
    const frame = decodeFrame(bytes);
    if (!frame) return;
    const previous = this.#frame;
    if (previous) {
      const step = (frame.counter - previous.counter + 0x1000000) % 0x1000000;
      if (step > 1) this.#dropped += step - 1;
    }
    this.#frame = frame;
    for (const run of [...this.#subscribers]) run(frame);
  }
}

export const telemetryStore = new TelemetryStore();
