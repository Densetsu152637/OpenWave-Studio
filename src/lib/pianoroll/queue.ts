/**
 * Serializes whole-state edits. The piano roll replaces the clip's content on every edit, so
 * two edits must never be built from the same stale clip. While one edit is being sent (and its
 * result is landing), further states are not sent: only the latest is kept and goes out as one
 * follow-up when the previous one is done. Three rapid nudges therefore send the first at once
 * and the third (which already contains the second) afterwards.
 */
export class EditQueue<T> {
  private sending = false;
  private dirty = false;
  private latest: T | undefined;
  private waiters: (() => void)[] = [];

  /**
   * `send` delivers one state and resolves when it is acknowledged and applied; `onIdle` runs
   * when nothing is left to send.
   */
  constructor(
    private readonly send: (state: T) => Promise<void>,
    private readonly onIdle: () => void = () => {},
  ) {}

  /** True while a state is being sent or waits to be. */
  get busy(): boolean {
    return this.sending;
  }

  /** Queues `state` as the newest; it replaces any state still waiting. */
  push(state: T): void {
    this.latest = state;
    if (this.sending) {
      this.dirty = true;
      return;
    }
    void this.drain();
  }

  /** Resolves when nothing is being sent or waiting. */
  idle(): Promise<void> {
    return this.sending ? new Promise((resolve) => this.waiters.push(resolve)) : Promise.resolve();
  }

  private async drain(): Promise<void> {
    this.sending = true;
    try {
      do {
        this.dirty = false;
        try {
          await this.send(this.latest as T);
        } catch {
          // The sender reports its own failures; the queue goes on with what is newest.
        }
      } while (this.dirty);
    } finally {
      this.sending = false;
    }
    this.onIdle();
    for (const resolve of this.waiters.splice(0)) resolve();
  }
}
