import { deviceDescriptors } from "../ipc/commands";
import type { DeviceDescriptor } from "../ipc/types";

/** The kind of the built-in synth, the instrument of a new MIDI track. */
export const SYNTH_KIND = "builtin.synth";

let loaded: Promise<DeviceDescriptor[]> | null = null;

/**
 * The device descriptors (built-in devices, then scanned plugins), fetched once and reused until
 * `resetDescriptors` (the plugin list changed). A failed fetch is not remembered, so the next
 * call tries again.
 */
export function loadDescriptors(
  fetch: () => Promise<DeviceDescriptor[]> = () => deviceDescriptors(),
): Promise<DeviceDescriptor[]> {
  loaded ??= fetch().catch((error) => {
    loaded = null;
    throw error;
  });
  return loaded;
}

/** Forgets the fetched descriptors: after a plugin scan, and in tests. */
export function resetDescriptors(): void {
  loaded = null;
}

/** The kinds that can be a MIDI track's instrument, built-in first (the backend's order). */
export const instrumentDescriptors = (descriptors: readonly DeviceDescriptor[]): DeviceDescriptor[] =>
  descriptors.filter((d) => d.instrument);

/** The descriptors of scanned plugins: those with a format. */
export const pluginDescriptors = (descriptors: readonly DeviceDescriptor[]): DeviceDescriptor[] =>
  descriptors.filter((d) => d.format !== undefined);

/** The text of a menu entry: the plugin's name, with its vendor when it has one. */
export const deviceLabel = (descriptor: Pick<DeviceDescriptor, "name" | "vendor">): string =>
  descriptor.vendor ? `${descriptor.name} (${descriptor.vendor})` : descriptor.name;

/**
 * Calls `onChange` whenever the backend's plugin list changes (a scan finished). Resolves with
 * the function that stops listening; outside the app (tests, a plain browser) there is nothing to
 * listen to and it resolves with a no-op.
 */
export async function watchPlugins(onChange: () => void): Promise<() => void> {
  try {
    const { listen } = await import("@tauri-apps/api/event");
    return await listen("plugins-changed", () => onChange());
  } catch {
    return () => {};
  }
}
