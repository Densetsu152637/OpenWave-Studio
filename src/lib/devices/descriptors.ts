import { deviceDescriptors } from "../ipc/commands";
import type { DeviceDescriptor } from "../ipc/types";

/** The kind of the built-in synth, the instrument of a new MIDI track. */
export const SYNTH_KIND = "builtin.synth";

let loaded: Promise<DeviceDescriptor[]> | null = null;

/**
 * The built-in device descriptors, fetched once per session (they cannot change while the app
 * runs). A failed fetch is not remembered, so the next call tries again.
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

/** Forgets the fetched descriptors (tests). */
export function resetDescriptors(): void {
  loaded = null;
}
