<script lang="ts">
  import { formatParam, panelGroups, positionToValue } from "./params";
  import type { PanelControl } from "./params";
  import type { Device, DeviceDescriptor, ParamDescriptorDto } from "../ipc/types";

  /**
   * The generic panel of one device: a slider per continuous parameter and a selector per choice
   * (a row of buttons for a short list), generated from the descriptor. A device whose parameters
   * name sections gets one collapsible section each (the first three open). It edits nothing
   * itself; `onParam` is called with the new value (once, when a slider is released). A plugin
   * whose editor this platform can show also gets an "Open editor" button, which calls
   * `onEditor`.
   */
  let {
    device,
    descriptor,
    onParam,
    onEditor,
  }: {
    device: Device;
    descriptor: DeviceDescriptor;
    onParam: (param: ParamDescriptorDto, value: number) => void;
    onEditor?: () => void;
  } = $props();

  /** The slider being dragged and the value it shows meanwhile. */
  let sliding = $state<{ id: number; value: number } | null>(null);

  const groups = $derived(panelGroups(device, descriptor));
  const sectioned = $derived(groups.length > 1 || (groups[0]?.name ?? "") !== "");
  /** Sections that start open. */
  const OPEN_SECTIONS = 3;
</script>

{#snippet row(control: PanelControl)}
  {@const param = control.param}
  {#if control.segmented}
    <div class="control" role="group" aria-label={param.name}>
      <span class="name">{control.label}</span>
      <div class="segments">
        {#each param.choices as label, index (index)}
          <button
            type="button"
            class="segment"
            class:on={control.value === index}
            aria-pressed={control.value === index}
            onclick={() => onParam(param, index)}>{label}</button
          >
        {/each}
      </div>
    </div>
  {:else}
    <label class="control">
      <span class="name">{control.label}</span>
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
  {/if}
{/snippet}

<section class="device" aria-label="{device.name} panel">
  <h3>{device.name}</h3>
  {#if descriptor.editor && onEditor}
    <button type="button" class="editor" onclick={() => onEditor()}>Open editor</button>
  {/if}
  {#if sectioned}
    {#each groups as group, index (group.name)}
      <details class="group" open={index < OPEN_SECTIONS}>
        <summary>{group.name || "Other"}</summary>
        <div class="controls">
          {#each group.controls as control (control.param.id)}
            {@render row(control)}
          {/each}
        </div>
      </details>
    {/each}
  {:else}
    <div class="controls">
      {#each groups[0]?.controls ?? [] as control (control.param.id)}
        {@render row(control)}
      {/each}
    </div>
  {/if}
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
  .editor {
    margin: 0 0 8px;
    font-size: 12px;
  }
  .group {
    border-top: 1px solid var(--border);
    padding: 4px 0;
  }
  summary {
    cursor: pointer;
    font-size: 11px;
    font-weight: 600;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 2px 0 4px;
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
  .segments {
    grid-column: 2 / 4;
    display: flex;
    gap: 2px;
  }
  .segment {
    flex: 1;
    min-width: 0;
    padding: 2px 4px;
    font-size: 11px;
    border: 1px solid var(--border);
    background: transparent;
    color: var(--muted);
    cursor: pointer;
  }
  .segment.on {
    color: inherit;
    background: var(--border);
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
