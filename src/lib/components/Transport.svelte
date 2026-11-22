<script lang="ts">
  import type { EngineStatus } from "../ipc";

  let { status, onToggle }: { status: EngineStatus; onToggle: (on: boolean) => void } = $props();
</script>

<div class="transport" role="toolbar" aria-label="Transport">
  {#if status.projectPlan}
    <!-- The engine plays the project now; the settings test tone no longer exists. -->
    <span class="readout">Playing the project</span>
  {:else}
  <button
    type="button"
    class="play"
    disabled={!status.running || status.testTone}
    onclick={() => onToggle(true)}
  >
    Play test tone
  </button>
  <button
    type="button"
    class="stop"
    disabled={!status.running || !status.testTone}
    onclick={() => onToggle(false)}
  >
    Stop
  </button>
  <span class="readout" aria-live="polite">{status.testTone ? "Playing 440 Hz" : "Stopped"}</span>
  {/if}
</div>

<style>
  .transport {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px;
    background: var(--panel-2);
    border-bottom: 1px solid var(--border);
  }
  .readout {
    color: var(--muted);
    margin-left: 8px;
  }
</style>
