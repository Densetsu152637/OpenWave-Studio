<script lang="ts">
  import { formatCpu, formatDevice } from "../format";
  import type { EngineStatus } from "../ipc";

  let { status, engineError = null }: { status: EngineStatus; engineError?: string | null } =
    $props();
</script>

<footer class="statusbar">
  <span class="device" class:error={!status.running} title="Audio output device">
    {formatDevice(status)}
  </span>
  {#if engineError}
    <span class="error" role="alert" title="The audio engine is still playing the previous version of the project">
      Engine: {engineError}
    </span>
  {/if}
  <span class="cpu" title="Audio engine load">CPU {formatCpu(status.cpuLoad)}</span>
</footer>

<style>
  .statusbar {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    padding: 3px 8px;
    background: var(--panel);
    border-top: 1px solid var(--border);
    color: var(--muted);
  }
  .error {
    color: var(--danger);
  }
  .device {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
