<script lang="ts">
  import type { EngineStatus } from "../ipc";
  import { countInLabel, countInOptions } from "../record/ranges";

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
    countIn = 0,
    punchOn = false,
    punchSet = false,
    loopOn = false,
    onCountIn,
    onPunch,
    onLoop,
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
    /** Count-in bars before recording. */
    countIn?: number;
    /** Recording is limited to the punch range. */
    punchOn?: boolean;
    /** A punch range exists (it is set on the ruler's punch strip). */
    punchSet?: boolean;
    /** The project has a loop range. */
    loopOn?: boolean;
    onCountIn?: (bars: number) => void;
    onPunch?: () => void;
    onLoop?: () => void;
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
    {#if status.countingIn}
      <span class="counting" role="status">Count-in</span>
    {/if}
    <span class="sep" aria-hidden="true"></span>
    <button
      type="button"
      class="toggle"
      class:on={loopOn}
      aria-pressed={loopOn}
      disabled={recording}
      title="Loop: recording passes of the loop range become takes (Shift+drag the ruler to set the range)"
      onclick={() => onLoop?.()}
    >
      Loop
    </button>
    <button
      type="button"
      class="toggle"
      class:on={punchOn}
      aria-pressed={punchOn}
      disabled={recording || !punchSet}
      title={punchSet
        ? "Punch: record only inside the punch range"
        : "Drag on the ruler's punch strip to set a punch range"}
      onclick={() => onPunch?.()}
    >
      Punch
    </button>
    <label class="count-in" title="Bars of metronome before recording starts">
      Count-in
      <select
        aria-label="Count-in bars"
        value={String(countIn)}
        disabled={recording}
        onchange={(event) => onCountIn?.(Number(event.currentTarget.value))}
      >
        {#each countInOptions(countIn) as bars (bars)}
          <option value={String(bars)}>{countInLabel(bars)}</option>
        {/each}
      </select>
    </label>
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
  .counting {
    color: var(--danger);
    font-weight: 600;
  }
  .sep {
    width: 1px;
    align-self: stretch;
    background: var(--border);
    margin: 0 4px;
  }
  .toggle.on {
    background: var(--accent);
    color: var(--bg);
  }
  .count-in {
    display: flex;
    align-items: center;
    gap: 4px;
    color: var(--muted);
  }
  .readout {
    color: var(--muted);
    margin-left: 8px;
    font-variant-numeric: tabular-nums;
  }
</style>
