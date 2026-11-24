<script lang="ts">
  import { onMount } from "svelte";
  import { CHANNELS, describeStatus } from "../../../lib/update";
  import { updates } from "../../../lib/update/updates.svelte";

  onMount(() => updates.start());

  const status = $derived(updates.status);
  const busy = $derived(updates.busy);
  const line = $derived(
    updates.installing && status.state !== "downloading" && status.state !== "checking"
      ? "Installing…"
      : describeStatus(status),
  );
</script>

<section aria-labelledby="about-heading">
  <h3 id="about-heading">About</h3>
  <p>OpenWave Studio <strong data-testid="about-version">{status.currentVersion}</strong></p>
</section>

<section aria-labelledby="updates-heading">
  <h3 id="updates-heading">Updates</h3>
  <p>Installed <output>v{status.currentVersion}</output></p>
  <div class="channels" role="radiogroup" aria-label="Update channel">
    {#each CHANNELS as channel (channel.id)}
      <label class="radio" title={channel.hint}>
        <input
          type="radio"
          name="update-channel"
          checked={status.channel === channel.id}
          onchange={() => void updates.setChannel(channel.id)}
          disabled={busy}
        />
        {channel.label}
        <span class="hint">{channel.hint}</span>
      </label>
    {/each}
  </div>
  <p class="status" role="status" data-testid="update-status" data-state={status.state}>{line}</p>
  {#if updates.error}
    <p class="error" role="alert">{updates.error}</p>
  {/if}
  <button type="button" onclick={() => void updates.installNow()} disabled={busy}>
    {busy && status.state !== "disabled" ? "Updating…" : "Check and install updates"}
  </button>
</section>

<style>
  h3 {
    margin: 12px 0 6px;
  }
  .channels {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin: 6px 0;
  }
  .radio {
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  .hint,
  .status {
    color: var(--muted);
  }
  .hint {
    font-size: 0.9em;
  }
  .status {
    margin: 6px 0;
  }
  .error {
    color: var(--danger);
  }
</style>
