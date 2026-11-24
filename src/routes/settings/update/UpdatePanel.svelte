<script lang="ts">
  import { onMount } from "svelte";
  import {
    CHANNELS,
    canCheck,
    describeStatus,
    offlineStatus,
    onUpdateStatus,
    updateCheck,
    updateRestart,
    updateSetChannel,
    updateStatus,
    type Channel,
    type UpdateStatus,
  } from "../../../lib/update";

  let status = $state<UpdateStatus>(offlineStatus);
  let error = $state<string | null>(null);

  const disabled = $derived(status.state === "disabled");

  onMount(() => {
    let unlisten: (() => void) | undefined;
    let closed = false;
    void (async () => {
      try {
        status = await updateStatus();
        const stop = await onUpdateStatus((next) => (status = next));
        if (closed) stop();
        else unlisten = stop;
      } catch (cause) {
        error = String(cause);
      }
    })();
    return () => {
      closed = true;
      unlisten?.();
    };
  });

  async function run(action: () => Promise<void>) {
    error = null;
    try {
      await action();
    } catch (cause) {
      error = String(cause);
    }
  }

  const checkNow = () => run(updateCheck);
  const restartNow = () => run(updateRestart);
  const switchChannel = (event: Event) =>
    run(async () => {
      status = await updateSetChannel((event.currentTarget as HTMLSelectElement).value as Channel);
    });
</script>

<section aria-labelledby="about-heading">
  <h3 id="about-heading">About</h3>
  <p>OpenWave Studio <strong data-testid="about-version">{status.currentVersion}</strong></p>
</section>

<section aria-labelledby="updates-heading">
  <h3 id="updates-heading">Updates</h3>
  <label class="row">
    Update channel
    <select value={status.channel} onchange={switchChannel} {disabled}>
      {#each CHANNELS as channel (channel.id)}
        <option value={channel.id}>{channel.label}</option>
      {/each}
    </select>
  </label>
  <div class="row">
    <button type="button" onclick={checkNow} disabled={!canCheck(status)}>Check now</button>
    {#if status.state === "ready"}
      <button type="button" onclick={restartNow}>Restart now</button>
    {/if}
  </div>
  <p class="status" role="status" data-testid="update-status">{describeStatus(status)}</p>
  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}
</section>

<style>
  h3 {
    margin: 12px 0 6px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 6px 0;
  }
  select {
    font: inherit;
    color: inherit;
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 3px 6px;
  }
  .status {
    color: var(--muted);
    margin: 6px 0;
  }
  .error {
    color: var(--danger);
  }
</style>
