<script lang="ts">
  import { onMount } from "svelte";
  import { updates } from "../update/updates.svelte";

  let { onOpen }: { onOpen: () => void } = $props();

  onMount(() => updates.start());
</script>

{#if updates.available}
  {#if updates.status.state === "ready"}
    <button type="button" class="update-ready" onclick={() => void updates.restart()}>Restart to update</button>
  {/if}
  {#if updates.error}
    <span class="update-error" role="alert" title={updates.error}>Update failed</span>
  {/if}
  <button type="button" class="update-version" title="Updates" onclick={onOpen} data-testid="update-version">
    v{updates.status.currentVersion}
    {#if updates.status.channel === "experimental"}
      <span class="channel-tag">experimental</span>
    {/if}
  </button>
{/if}

<style>
  .update-version {
    background: transparent;
    border-color: transparent;
    padding: 3px 8px;
    color: var(--muted);
  }
  .update-ready {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--bg);
    padding: 3px 10px;
  }
  .update-error {
    color: var(--danger);
    padding: 0 4px;
  }
  .channel-tag {
    margin-left: 4px;
    padding: 0 5px;
    border: 1px solid var(--border);
    border-radius: 8px;
    font-size: 0.85em;
  }
</style>
