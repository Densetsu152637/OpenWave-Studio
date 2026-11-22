import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Channel = "main" | "experimental";

export const CHANNELS: readonly { id: Channel; label: string }[] = [
  { id: "main", label: "Main (stable)" },
  { id: "experimental", label: "Experimental" },
];

/** Mirrors `UpdateStatus` in src-tauri/src/update.rs (camelCase over IPC, state tagged). */
export type UpdateStatus = {
  currentVersion: string;
  channel: Channel;
  /** Whether the app checks by itself (off in development builds and when updates are disabled). */
  automatic: boolean;
} & (
  | { state: "idle" }
  | { state: "checking" }
  | { state: "up_to_date" }
  | { state: "downloading"; version: string }
  | { state: "ready"; version: string; notes: string | null }
  | { state: "failed"; message: string }
  | { state: "disabled"; reason: "package_managed" | "translocated"; message: string }
);

export const offlineStatus: UpdateStatus = {
  currentVersion: "unknown",
  channel: "main",
  automatic: false,
  state: "idle",
};

/** One line describing the update state for the Updates panel. */
export function describeStatus(status: UpdateStatus): string {
  switch (status.state) {
    case "idle":
      return status.automatic ? "Waiting for the next check." : "Updates are checked when you ask.";
    case "checking":
      return "Checking for updates...";
    case "up_to_date":
      return "OpenWave Studio is up to date.";
    case "downloading":
      return `Downloading ${status.version}...`;
    case "ready":
      return `Update ready: ${status.version}. It installs when you quit, or restart now.`;
    case "failed":
      return status.message;
    case "disabled":
      return status.message;
  }
}

/** Whether "Check now" does anything in this state. */
export function canCheck(status: UpdateStatus): boolean {
  return status.state !== "checking" && status.state !== "downloading" && status.state !== "disabled";
}

export async function updateStatus(): Promise<UpdateStatus> {
  if (!isTauri()) return offlineStatus;
  return invoke<UpdateStatus>("update_status");
}

export async function updateCheck(): Promise<void> {
  if (!isTauri()) return;
  await invoke("update_check");
}

export async function updateSetChannel(channel: Channel): Promise<UpdateStatus> {
  if (!isTauri()) return { ...offlineStatus, channel };
  return invoke<UpdateStatus>("update_set_channel", { channel });
}

export async function updateRestart(): Promise<void> {
  if (!isTauri()) return;
  await invoke("update_restart");
}

/** Calls `handler` for every status change the backend pushes. */
export async function onUpdateStatus(handler: (status: UpdateStatus) => void): Promise<UnlistenFn> {
  if (!isTauri()) return () => {};
  return listen<UpdateStatus>("update://status", (event) => handler(event.payload));
}
