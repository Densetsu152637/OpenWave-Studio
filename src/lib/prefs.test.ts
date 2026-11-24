import { describe, expect, it } from "vitest";
import { defaultPreferences, type DeviceInfo, type Preferences } from "./ipc";
import {
  SHORTCUT_ACTIONS,
  acceleratorFromEvent,
  applyTheme,
  bufferChoices,
  bufferMs,
  effectiveShortcut,
  samePrefs,
  selectedOutput,
  setMidiInput,
  shortcutConflicts,
  withShortcut,
} from "./prefs";

const prefs = (): Preferences => structuredClone(defaultPreferences);

const device = (overrides: Partial<DeviceInfo> = {}): DeviceInfo => ({
  id: "d1",
  name: "Speakers",
  isDefaultOutput: false,
  isDefaultInput: false,
  maxOutputChannels: 2,
  maxInputChannels: 0,
  sampleRates: [44100, 48000],
  minBufferFrames: null,
  maxBufferFrames: null,
  ...overrides,
});

describe("applyTheme", () => {
  it("sets and clears the data-theme attribute", () => {
    const attributes = new Map<string, string>();
    const root = {
      setAttribute: (name: string, value: string) => void attributes.set(name, value),
      removeAttribute: (name: string) => void attributes.delete(name),
    };
    applyTheme("light", root);
    expect(attributes.get("data-theme")).toBe("light");
    applyTheme("dark", root);
    expect(attributes.get("data-theme")).toBe("dark");
    applyTheme("system", root);
    expect(attributes.has("data-theme")).toBe(false);
  });
});

describe("audio helpers", () => {
  it("offers buffer sizes within the device range", () => {
    expect(bufferChoices(undefined)).toEqual([32, 64, 128, 256, 512, 1024, 2048]);
    expect(bufferChoices(device({ minBufferFrames: 64, maxBufferFrames: 512 }))).toEqual([64, 128, 256, 512]);
  });

  it("formats buffer latency", () => {
    expect(bufferMs(48, 48000)).toBe("1.0 ms");
    expect(bufferMs(256, null)).toBe("5.3 ms");
  });

  it("falls back to the default output when the stored one is gone", () => {
    const list = [device({ id: "a" }), device({ id: "b", isDefaultOutput: true })];
    const p = prefs();
    p.audio.outputDevice = "missing";
    expect(selectedOutput(p, list)?.id).toBe("b");
    p.audio.outputDevice = "a";
    expect(selectedOutput(p, list)?.id).toBe("a");
  });
});

describe("setMidiInput", () => {
  it("adds, updates and keeps other inputs", () => {
    let inputs = setMidiInput([], "p1", "Keys", {});
    expect(inputs).toEqual([{ port: "p1", name: "Keys", enabled: true, controlSurface: false }]);
    inputs = setMidiInput(inputs, "p2", "Pads", { controlSurface: true });
    inputs = setMidiInput(inputs, "p1", "Keys", { enabled: false });
    expect(inputs.map((i) => [i.port, i.enabled, i.controlSurface])).toEqual([
      ["p1", false, false],
      ["p2", true, true],
    ]);
  });
});

describe("shortcuts", () => {
  it("builds accelerators from key events", () => {
    const base = { ctrlKey: false, shiftKey: false, altKey: false, metaKey: false };
    expect(acceleratorFromEvent({ ...base, key: " " })).toBe("Space");
    expect(acceleratorFromEvent({ ...base, key: "z", ctrlKey: true, shiftKey: true })).toBe("Ctrl+Shift+Z");
    expect(acceleratorFromEvent({ ...base, key: "ArrowLeft", altKey: true })).toBe("Alt+ArrowLeft");
    expect(acceleratorFromEvent({ ...base, key: "Shift", shiftKey: true })).toBeNull();
  });

  it("stores only overrides and reports conflicts", () => {
    const play = SHORTCUT_ACTIONS.find((a) => a.id === "transport.play")!;
    const stop = SHORTCUT_ACTIONS.find((a) => a.id === "transport.stop")!;
    let p = withShortcut(prefs(), play, "P");
    expect(p.shortcuts).toEqual({ "transport.play": "P" });
    expect(effectiveShortcut(p, play)).toBe("P");
    expect(effectiveShortcut(p, stop)).toBe("Enter");
    expect(shortcutConflicts(p, "transport.stop", "p").map((a) => a.id)).toEqual(["transport.play"]);
    p = withShortcut(p, play, "");
    expect(effectiveShortcut(p, play)).toBe("");
    p = withShortcut(p, play, play.default);
    expect(p.shortcuts).toEqual({});
  });
});

describe("samePrefs", () => {
  it("compares by value", () => {
    const a = prefs();
    const b = prefs();
    expect(samePrefs(a, b)).toBe(true);
    b.audio.bufferFrames = 128;
    expect(samePrefs(a, b)).toBe(false);
  });
});
