import { invoke, isTauri } from "@tauri-apps/api/core";

/** Mirrors `EngineStatus` in src-tauri/src/engine.rs (camelCase over IPC). */
export interface EngineStatus {
  running: boolean;
  deviceName: string | null;
  sampleRate: number | null;
  channels: number | null;
  testTone: boolean;
  /** Smoothed audio callback load, 0..1. */
  cpuLoad: number;
  error: string | null;
}

export const offlineStatus: EngineStatus = {
  running: false,
  deviceName: null,
  sampleRate: null,
  channels: null,
  testTone: false,
  cpuLoad: 0,
  error: "Not running inside OpenWave Studio",
};

export async function engineStatus(): Promise<EngineStatus> {
  if (!isTauri()) return offlineStatus;
  return invoke<EngineStatus>("engine_status");
}

export async function setTestTone(on: boolean): Promise<EngineStatus> {
  if (!isTauri()) return offlineStatus;
  return invoke<EngineStatus>("set_test_tone", { on });
}
