import { invoke, isTauri } from "@tauri-apps/api/core";

/** Mirrors `EngineStatus` in src-tauri/src/engine.rs (camelCase over IPC). */
export interface EngineStatus {
  running: boolean;
  deviceName: string | null;
  sampleRate: number | null;
  channels: number | null;
  testTone: boolean;
  /** A project plan is live: the engine plays the project, so the test tone is unavailable. */
  projectPlan: boolean;
  /** Smoothed audio callback load, 0..1. */
  cpuLoad: number;
  /** Why audio is not running (also while reconnecting), or null. */
  error: string | null;
  audioState: "running" | "recovering" | "failed";
  recoveryAttempt: number;
  recoveryMax: number;
  /** Successful device reopenings since launch. */
  recoveries: number;
  backend: string | null;
  bufferFrames: number | null;
  exclusive: boolean;
  /** Running on the default device because the chosen one could not be opened. */
  onFallbackDevice: boolean;
  midi: MidiStatus;
}

export interface MidiPortStatus {
  port: string;
  connected: boolean;
  error: string | null;
}

export interface MidiStatus {
  ports: MidiPortStatus[];
  events: number;
  dropped: number;
  /** Last message as [status, data1, data2]. */
  last: [number, number, number] | null;
}

export const offlineStatus: EngineStatus = {
  running: false,
  deviceName: null,
  sampleRate: null,
  channels: null,
  testTone: false,
  projectPlan: false,
  cpuLoad: 0,
  error: "Not running inside OpenWave Studio",
  audioState: "failed",
  recoveryAttempt: 0,
  recoveryMax: 0,
  recoveries: 0,
  backend: null,
  bufferFrames: null,
  exclusive: false,
  onFallbackDevice: false,
  midi: { ports: [], events: 0, dropped: 0, last: null },
};

export async function engineStatus(): Promise<EngineStatus> {
  if (!isTauri()) return offlineStatus;
  return invoke<EngineStatus>("engine_status");
}

export async function setTestTone(on: boolean): Promise<EngineStatus> {
  if (!isTauri()) return offlineStatus;
  return invoke<EngineStatus>("set_test_tone", { on });
}

/** Mirrors `Preferences` in src-tauri/src/prefs.rs (camelCase over IPC). */
export interface Preferences {
  version: number;
  audio: {
    /** Backend id; empty selects the platform default. */
    backend: string;
    outputDevice: string | null;
    inputDevice: string | null;
    sampleRate: number | null;
    bufferFrames: number | null;
  };
  midi: { inputs: MidiInputPref[] };
  paths: { projectsDir: string | null; pluginDirs: string[] };
  appearance: { theme: Theme };
  /** Control by AI agents through the local MCP endpoint. */
  ai: { allowControl: boolean };
  /** Overrides of the default shortcuts: action id to accelerator ("" unbinds). */
  shortcuts: Record<string, string>;
}

export type Theme = "system" | "light" | "dark";

export interface MidiInputPref {
  port: string;
  name: string;
  enabled: boolean;
  controlSurface: boolean;
}

export interface PrefsSnapshot {
  prefs: Preferences;
  /** Why defaults are in use when the stored file could not be loaded. */
  warning: string | null;
  configFile: string;
}

export interface PrefsApplied {
  prefs: Preferences;
  /** Why the requested audio device was not applied. */
  audioError: string | null;
  status: EngineStatus;
}

export interface DeviceInfo {
  id: string;
  name: string;
  isDefaultOutput: boolean;
  isDefaultInput: boolean;
  maxOutputChannels: number;
  maxInputChannels: number;
  sampleRates: number[];
  minBufferFrames: number | null;
  maxBufferFrames: number | null;
}

export interface BackendInfo {
  id: string;
  label: string;
  devices: DeviceInfo[];
  error: string | null;
}

export interface DeviceLists {
  backends: BackendInfo[];
  midiInputs: { id: string; name: string }[];
  midiError: string | null;
  /** Device id that means "system default". */
  defaultDevice: string;
}

export const defaultPreferences: Preferences = {
  version: 1,
  audio: { backend: "", outputDevice: null, inputDevice: null, sampleRate: null, bufferFrames: null },
  midi: { inputs: [] },
  paths: { projectsDir: null, pluginDirs: [] },
  appearance: { theme: "system" },
  ai: { allowControl: true },
  shortcuts: {},
};

export const noDevices: DeviceLists = {
  backends: [],
  midiInputs: [],
  midiError: null,
  defaultDevice: "default",
};

export async function prefsGet(): Promise<PrefsSnapshot> {
  if (!isTauri()) return { prefs: structuredClone(defaultPreferences), warning: null, configFile: "" };
  return invoke<PrefsSnapshot>("prefs_get");
}

export async function prefsSet(prefs: Preferences): Promise<PrefsApplied> {
  if (!isTauri()) return { prefs, audioError: null, status: offlineStatus };
  return invoke<PrefsApplied>("prefs_set", { prefs });
}

export async function audioDevices(): Promise<DeviceLists> {
  if (!isTauri()) return noDevices;
  return invoke<DeviceLists>("audio_devices");
}

export async function audioRetry(): Promise<EngineStatus> {
  if (!isTauri()) return offlineStatus;
  return invoke<EngineStatus>("audio_retry");
}
