<script lang="ts">
  import { MAX_DB, MIN_DB, dbLabel, outputValue, panLabel, parseOutput, snapPan } from "../mixer/mixer";
  import type { Bus, Output, StripSettings } from "../ipc/types";

  /**
   * The fader, pan and output controls of one strip (a track, a bus or the master). It edits
   * nothing itself: sliders report their value when released.
   */
  let {
    settings,
    destinations,
    routable,
    onVolume,
    onPan,
    onOutput,
  }: {
    settings: StripSettings;
    /** The buses the strip's output may be set to (no routing cycle). */
    destinations: readonly Bus[];
    /** The strip has an output to choose (false for the master). */
    routable: boolean;
    onVolume: (gainDb: number) => void;
    onPan: (pan: number) => void;
    onOutput: (output: Output) => void;
  } = $props();

  let sliding = $state<number | null>(null);
  let panning = $state<number | null>(null);

  const gain = $derived(sliding ?? settings.gain_db);
  const pan = $derived(panning ?? settings.pan);
</script>

<div class="row">
  <input
    class="volume"
    type="range"
    aria-label="Volume"
    min={MIN_DB}
    max={MAX_DB}
    step="0.1"
    value={gain}
    oninput={(event) => (sliding = event.currentTarget.valueAsNumber)}
    onchange={(event) => {
      sliding = null;
      onVolume(event.currentTarget.valueAsNumber);
    }}
  />
  <span class="db">{dbLabel(gain)}</span>
</div>
<div class="row">
  <input
    class="pan"
    type="range"
    aria-label="Pan"
    title="Pan (double-click to centre)"
    min="-1"
    max="1"
    step="0.01"
    value={pan}
    oninput={(event) => (panning = event.currentTarget.valueAsNumber)}
    onchange={(event) => {
      const next = snapPan(event.currentTarget.valueAsNumber);
      panning = null;
      onPan(next);
    }}
    ondblclick={() => onPan(0)}
  />
  <span class="pan-label">{panLabel(pan)}</span>
  {#if routable}
    <select
      class="output"
      aria-label="Output"
      title="Output"
      value={outputValue(settings.output)}
      onchange={(event) => {
        const output = parseOutput(event.currentTarget.value);
        if (output !== null) onOutput(output);
      }}
    >
      <option value="master">Master</option>
      {#each destinations as bus (bus.id)}
        <option value={String(bus.id)}>{bus.name}</option>
      {/each}
    </select>
  {/if}
</div>

<style>
  .row {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .volume,
  .pan {
    flex: 1;
    min-width: 0;
  }
  .db {
    width: 58px;
    text-align: right;
    color: var(--muted);
  }
  .pan-label {
    width: 30px;
    text-align: right;
    color: var(--muted);
  }
  .output {
    width: 70px;
    min-width: 0;
  }
</style>
