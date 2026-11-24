// The updater's status and actions, shared by the menu bar and the Updates settings panel. The
// native side checks the chosen channel on its own and downloads newer builds; this store shows
// its status, switches channel, and installs on request.

import { isTauri } from "@tauri-apps/api/core";
import {
  canCheck,
  offlineStatus,
  onUpdateStatus,
  updateInstallNow,
  updateRestart,
  updateSetChannel,
  updateStatus,
  type Channel,
  type UpdateStatus,
} from "./index";

class Updates {
  status = $state<UpdateStatus>(offlineStatus);
  /** Set while a check-and-install the user asked for is running. */
  installing = $state(false);
  error = $state<string | null>(null);
  /** False outside the desktop app, where there is nothing to show. */
  readonly available = isTauri();
  #started = false;

  /** Whether an update action is running, so the controls are disabled. */
  get busy(): boolean {
    return this.installing || !canCheck(this.status);
  }

  /** Loads the status and follows every change the backend pushes. Safe to call more than once. */
  start(): void {
    if (this.#started || !this.available) return;
    this.#started = true;
    void onUpdateStatus((next) => (this.status = next)).catch(() => {});
    void updateStatus()
      .then((initial) => (this.status = initial))
      .catch((cause: unknown) => (this.error = String(cause)));
  }

  async #run(action: () => Promise<void>): Promise<void> {
    this.error = null;
    try {
      await action();
    } catch (cause) {
      this.error = String(cause);
    }
  }

  async setChannel(channel: Channel): Promise<void> {
    await this.#run(async () => {
      this.status = await updateSetChannel(channel);
    });
    // On failure the channel is unchanged; a fresh object re-checks the radio the user clicked away from.
    if (this.error) this.status = { ...this.status };
  }

  /** Checks now and installs and restarts if a build is found. */
  async installNow(): Promise<void> {
    this.installing = true;
    // On success the app restarts; otherwise the status says why nothing was installed.
    await this.#run(async () => {
      this.status = await updateInstallNow();
    });
    this.installing = false;
  }

  /** Installs an already downloaded build and restarts. */
  restart(): Promise<void> {
    return this.#run(updateRestart);
  }
}

export const updates = new Updates();
