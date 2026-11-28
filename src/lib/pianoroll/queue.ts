/**
 * Serializes whole-state edits, one queue entry per target (a clip id). The piano roll replaces
 * a clip's content on every edit, so two edits must never be built from the same stale clip.
 * While one edit is being sent (and its result is landing) further states are not sent: per
 * target only the latest is kept and goes out as one follow-up. Three rapid nudges therefore
 * send the first at once and the third (which already contains the second) afterwards. States
 * for different targets never replace each other, so switching clips loses nothing.
 */
export class EditQueue<K, T> {
  private sending = false;
  private pending = new Map<K, T>();
  private waiters: (() => void)[] = [];

  /**
   * `send` delivers one state to its target and resolves when it is applied; resolving to
   * `false` or throwing means it was refused, which drops that target's waiting follow-up (it
   * was built on content that never arrived) and calls `onFail`. `onIdle` runs when nothing is
   * left to send.
   */
  constructor(
    private readonly send: (key: K, state: T) => Promise<boolean | void>,
    private readonly hooks: { onIdle?: () => void; onFail?: (key: K) => void } = {},
  ) {}

  /** True while a state is being sent or waits to be. */
  get busy(): boolean {
    return this.sending;
  }

  /** Queues `state` as the newest for `key`; it replaces any state still waiting for `key`. */
  push(key: K, state: T): void {
    this.pending.set(key, state);
    void this.drain();
  }

  /** Resolves when nothing is being sent or waiting. */
  idle(): Promise<void> {
    return this.sending ? new Promise((resolve) => this.waiters.push(resolve)) : Promise.resolve();
  }

  private async drain(): Promise<void> {
    if (this.sending) return;
    this.sending = true;
    try {
      while (this.pending.size > 0) {
        const [key, state] = this.pending.entries().next().value as [K, T];
        this.pending.delete(key);
        let ok = true;
        try {
          ok = (await this.send(key, state)) !== false;
        } catch {
          ok = false;
        }
        if (!ok) {
          this.pending.delete(key);
          this.hooks.onFail?.(key);
        }
      }
    } finally {
      this.sending = false;
    }
    this.hooks.onIdle?.();
    // `onIdle` may have queued more; its drain resolves the waiters when it ends.
    if (!this.sending) for (const resolve of this.waiters.splice(0)) resolve();
  }
}
