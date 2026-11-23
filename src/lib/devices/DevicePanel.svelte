<script lang="ts">
  import { formatParam, panelControls, positionToValue } from "./params";
  import type { Device, DeviceDescriptor, ParamDescriptorDto } from "../ipc/types";

  /**
   * The generic panel of one device: a slider per continuous parameter and a selector per choice,
   * generated from the descriptor. It edits nothing itself; `onParam` is called with the new value
   * (once, when a slider is released).
   */
  let {
    device,
    descriptor,
    onParam,
  }: {
    device: Device;
    descriptor: DeviceDescriptor;
    onParam: (param: ParamDescriptorDto, value: number) => void;
  } = $props();

  /** The slider being dragged and the value it shows meanwhile. */
  let sliding = $state<{ id: number; value: number } | null>(null);

  const controls = $derived(panelControls(device, descriptor));
</script>

<section class="device" aria-label="{device.name} panel">
  <h3>{device.name}</h3>
  <div class="controls">
    {#each controls as control (control.param.id)}
      {@const param = control.param}
      <label class="control">
        <span class="name">{param.name}</span>
        {#if control.choice}
          <select
            value={control.value}
            onchange={(event) => onParam(param, Number(event.currentTarget.value))}
          >
            {#each param.choices as label, index (index)}
              <option value={index}>{label}</option>
            {/each}
          </select>
        {:else}
          <input
            type="range"
            min="0"
            max="1"
            step="0.001"
            aria-label={param.name}
            value={sliding?.id === param.id ? undefined : control.position}
            oninput={(event) =>
              (sliding = {
                id: param.id,
                value: positionToValue(param, Number(event.currentTarget.value)),
              })}
            onchange={(event) => {
              onParam(param, positionToValue(param, Number(event.currentTarget.value)));
              sliding = null;
            }}
            ondblclick={() => onParam(param, param.default)}
          />
          <span class="value">
            {sliding?.id === param.id ? formatParam(param, sliding.value) : control.text}
          </span>
        {/if}
      </label>
    {/each}
  </div>
</section>

<style>
  .device {
    box-sizing: border-box;
    width: 260px;
    flex: none;
    overflow-y: auto;
    padding: 6px 10px;
    border-left: 1px solid var(--border);
    background: var(--panel);
  }
  h3 {
    margin: 0 0 6px;
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .controls {
    display: grid;
    gap: 6px;
  }
  .control {
    display: grid;
    grid-template-columns: 72px 1fr 58px;
    align-items: center;
    gap: 6px;
    font-size: 12px;
  }
  .control select {
    grid-column: 2 / 4;
  }
  .value {
    text-align: right;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  input[type="range"] {
    min-width: 0;
    width: 100%;
  }
</style>
