import { describe, expect, it, vi } from "vitest";
import type { IpcTransport } from "../ipc/transport";
import { TelemetryStore } from "./telemetry";

function frame(counter: number, slots: number[]): ArrayBuffer {
  const words = [1, counter, 0, 480, 0.25, 0, 0, 0, slots.length];
  for (const slot of slots) words.push(slot, 0.5, 0.5, 0.1, 0.1);
  const bytes = new ArrayBuffer(words.length * 4);
  const view = new DataView(bytes);
  words.forEach((w, i) => view.setFloat32(i * 4, w, true));
  return bytes;
}

function mock() {
  let onmessage: (raw: ArrayBuffer | number[]) => void = () => {};
  const invoke = vi.fn(async (_command: string, args?: Record<string, unknown>) => {
    if (args && "frames" in args) onmessage = (args.frames as { onmessage: typeof onmessage }).onmessage;
  });
  const transport: IpcTransport = {
    available: true,
    channel: <T>(handler: (message: T) => void) => ({ onmessage: handler }),
    invoke: invoke as unknown as IpcTransport["invoke"],
  };
  return { transport, invoke, send: (raw: ArrayBuffer | number[]) => onmessage(raw) };
}

describe("telemetry store", () => {
  it("starts the bridge, decodes frames from the channel and counts dropped ones", async () => {
    const { transport, invoke, send } = mock();
    const store = new TelemetryStore(transport);
    const seen: (number | undefined)[] = [];
    store.subscribe((f) => seen.push(f?.counter));
    await store.start(60);
    expect(invoke).toHaveBeenCalledWith("telemetry_start", expect.objectContaining({ hz: 60 }));
    send(frame(1, [0, 3]));
    send(frame(2, [0, 3]));
    send(frame(5, [0, 3])); // 3 and 4 never arrived
    expect(seen).toEqual([undefined, 1, 2, 5]);
    expect(store.droppedFrames).toBe(2);
    expect(store.frame?.strips.map((s) => s.slot)).toEqual([0, 3]);
    expect(store.frame?.position).toBe(480);
    // A malformed frame is ignored; a plain number array (the JSON fallback) still decodes.
    send(new ArrayBuffer(6));
    expect(store.frame?.counter).toBe(5);
    send(Array.from(new Uint8Array(frame(6, []))));
    expect(store.frame?.counter).toBe(6);
  });

  it("wraps the frame counter without counting a drop", async () => {
    const { transport, send } = mock();
    const store = new TelemetryStore(transport);
    await store.start();
    send(frame(0xffffff, []));
    send(frame(0, []));
    expect(store.droppedFrames).toBe(0);
  });

  it("reports the visible strips and stops", async () => {
    const { transport, invoke } = mock();
    const store = new TelemetryStore(transport);
    await store.start();
    await store.setVisibleStrips([0, 2, 5]);
    expect(invoke).toHaveBeenCalledWith("telemetry_set_strips", { slots: [0, 2, 5] });
    await store.stop();
    expect(invoke).toHaveBeenCalledWith("telemetry_stop");
  });

  it("does nothing without a backend", async () => {
    const { transport, invoke } = mock();
    const store = new TelemetryStore({ ...transport, available: false });
    await store.start();
    await store.setVisibleStrips([1]);
    expect(invoke).not.toHaveBeenCalled();
  });
});
