<script lang="ts">
  import type { EngineStatus } from "../ipc";

  let {
    status,
    onToggle,
    playing,
    recording,
    canRecord,
    position,
    onPlay,
    onRecord,
    onStop,
  }: {
    status: EngineStatus;
    onToggle: (on: boolean) => void;
    playing: boolean;
    /** A recording is running (Stop ends it and adds the takes). */
    recording: boolean;
    /** At least one track is armed. */
    canRecord: boolean;
    /** "bar.beat" of the playhead. */
    position: string;
    onPlay: () => void;
    onRecord: () => void;
    onStop: () => void;
  } = $props();
</script>

<div class="transport" role="toolbar" aria-label="Transport">
  {#if status.projectPlan}
    <button type="button" class="play" disabled={playing} onclick={onPlay}>Play</button>
    <button
      type="button"
      class="record"
      class:on={recording}
      aria-pressed={recording}
      disabled={playing || !canRecord}
      title={canRecord ? "Record on the armed tracks" : "Arm an audio track to record"}
      onclick={onRecord}
    >
      Record
    </button>
    <button type="button" class="stop" onclick={onStop}>Stop</button>
    <span class="readout" aria-live="off" title="Position (bar.beat)">{position}</span>
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
  .record.on {
    background: var(--danger);
    color: var(--bg);
  }
  .readout {
    color: var(--muted);
    margin-left: 8px;
    font-variant-numeric: tabular-nums;
  }
</style>
