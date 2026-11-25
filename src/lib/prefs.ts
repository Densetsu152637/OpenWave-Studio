import type { DeviceInfo, DeviceLists, MidiInputPref, Preferences, Theme } from "./ipc";
import { ARRANGE_SHORTCUTS } from "./arrange/keys";

/** Limits mirrored from src-tauri/src/prefs.rs and ow-io. */
export const MIN_BUFFER_FRAMES = 16;
export const MAX_BUFFER_FRAMES = 4096;
/** Buffer sizes offered in the picker (a device may only honour some of them). */
export const BUFFER_CHOICES = [32, 64, 128, 256, 512, 1024, 2048];

/** Sets the theme on the document; "system" removes the override so the OS setting decides. */
export function applyTheme(
  theme: Theme,
  root: Pick<Element, "setAttribute" | "removeAttribute"> = document.documentElement,
): void {
  if (theme === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", theme);
}

/** The backend whose devices are shown: the chosen one, else the first available. */
export function activeBackend(prefs: Preferences, lists: DeviceLists) {
  return lists.backends.find((backend) => backend.id === prefs.audio.backend) ?? lists.backends[0];
}

/** Devices of a backend that can play audio. */
export function outputDevices(devices: DeviceInfo[]): DeviceInfo[] {
  return devices.filter((device) => device.maxOutputChannels > 0);
}

/** Devices of a backend that can record audio. */
export function inputDevices(devices: DeviceInfo[]): DeviceInfo[] {
  return devices.filter((device) => device.maxInputChannels > 0);
}

/** The selected output device, or the backend default one. */
export function selectedOutput(prefs: Preferences, devices: DeviceInfo[]): DeviceInfo | undefined {
  const outputs = outputDevices(devices);
  return (
    outputs.find((device) => device.id === prefs.audio.outputDevice) ??
    outputs.find((device) => device.isDefaultOutput)
  );
}

/** Buffer sizes to offer for a device: the standard choices within its reported range. */
export function bufferChoices(device: DeviceInfo | undefined): number[] {
  const min = device?.minBufferFrames ?? MIN_BUFFER_FRAMES;
  const max = device?.maxBufferFrames ?? MAX_BUFFER_FRAMES;
  return BUFFER_CHOICES.filter((frames) => frames >= min && frames <= max);
}

/** Latency of one buffer in milliseconds, for display. */
export function bufferMs(frames: number, sampleRate: number | null): string {
  const rate = sampleRate && sampleRate > 0 ? sampleRate : 48000;
  return `${((frames / rate) * 1000).toFixed(1)} ms`;
}

/** Adds or updates a MIDI input entry, keeping the others untouched. */
export function setMidiInput(
  inputs: MidiInputPref[],
  port: string,
  name: string,
  change: Partial<Pick<MidiInputPref, "enabled" | "controlSurface">>,
): MidiInputPref[] {
  const existing = inputs.find((input) => input.port === port);
  if (!existing) {
    return [...inputs, { port, name, enabled: true, controlSurface: false, ...change }];
  }
  return inputs.map((input) => (input.port === port ? { ...input, ...change } : input));
}

/** A rebindable action and its default accelerator. */
export interface ShortcutAction {
  id: string;
  label: string;
  default: string;
  /** Not remappable: the feature reads this key directly. */
  fixed?: boolean;
}

/** Actions that exist today (the arrangement ones are handled by the arrangement view). */
export const SHORTCUT_ACTIONS: ShortcutAction[] = [
  { id: "transport.play", label: "Play / pause", default: "Space" },
  { id: "transport.stop", label: "Stop", default: "Enter" },
  { id: "transport.record", label: "Record", default: "R" },
  { id: "edit.undo", label: "Undo", default: "Ctrl+Z" },
  { id: "edit.redo", label: "Redo", default: "Ctrl+Shift+Z" },
  { id: "file.save", label: "Save project", default: "Ctrl+S" },
  { id: "file.open", label: "Open project", default: "Ctrl+O" },
  { id: "app.settings", label: "Open settings", default: "Ctrl+," },
  ...ARRANGE_SHORTCUTS.map(({ id, label, default: accelerator }) => ({
    id,
    label,
    default: accelerator,
    fixed: true,
  })),
];

/** The accelerator in effect for an action: the override (possibly empty) or the default. */
export function effectiveShortcut(prefs: Preferences, action: ShortcutAction): string {
  return action.id in prefs.shortcuts ? prefs.shortcuts[action.id] : action.default;
}

const MODIFIER_KEYS = new Set(["Control", "Shift", "Alt", "Meta", "AltGraph"]);

/** Accelerator text for a key event, such as "Ctrl+Shift+Z"; null for a bare modifier press. */
export function acceleratorFromEvent(event: {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}): string | null {
  if (MODIFIER_KEYS.has(event.key)) return null;
  const parts: string[] = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  if (event.metaKey) parts.push("Meta");
  const key = event.key === " " ? "Space" : event.key.length === 1 ? event.key.toUpperCase() : event.key;
  parts.push(key);
  return parts.join("+");
}

/** Other actions that already use `accelerator` (case-insensitive), for conflict warnings. */
export function shortcutConflicts(
  prefs: Preferences,
  actionId: string,
  accelerator: string,
  actions: ShortcutAction[] = SHORTCUT_ACTIONS,
): ShortcutAction[] {
  if (!accelerator) return [];
  const wanted = accelerator.toLowerCase();
  return actions.filter(
    (action) => action.id !== actionId && effectiveShortcut(prefs, action).toLowerCase() === wanted,
  );
}

/** Sets an override, or drops it when it equals the default, so the file only holds changes. */
export function withShortcut(prefs: Preferences, action: ShortcutAction, accelerator: string): Preferences {
  const shortcuts = { ...prefs.shortcuts };
  if (accelerator === action.default) delete shortcuts[action.id];
  else shortcuts[action.id] = accelerator;
  return { ...prefs, shortcuts };
}

/** True when two preference sets are equal (they are plain JSON). */
export function samePrefs(a: Preferences, b: Preferences): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}
