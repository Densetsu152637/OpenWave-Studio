import { Channel, invoke, isTauri } from "@tauri-apps/api/core";

/**
 * The two Tauri primitives the IPC wrappers and stores need, so tests can supply a mock instead of
 * a webview. `channel` creates an object to pass as a command argument; the Rust side sends
 * messages to it and they arrive in `onmessage`.
 */
export interface IpcTransport {
  /** True when a backend is reachable (false in a plain browser). */
  readonly available: boolean;
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  channel<T>(onmessage: (message: T) => void): unknown;
}

/** The real transport over Tauri's `invoke` and `Channel`. */
export const tauriTransport: IpcTransport = {
  get available() {
    return isTauri();
  },
  invoke: (command, args) => invoke(command, args),
  channel: (onmessage) => new Channel(onmessage),
};
