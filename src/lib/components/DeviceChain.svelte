<script lang="ts">
  import type { ChainEntry } from "../devices/chain";
  import type { Device, DeviceDescriptor } from "../ipc/types";

  /**
   * The FX list of one track: its instrument (MIDI tracks) and inserts in signal order. Selecting
   * an entry shows its panel; an insert has a bypass toggle and a remove button; the menu adds one
   * of the built-in effects at the end. It edits nothing itself.
   */
  let {
    chain,
    shownId,
    effects,
    onSelect,
    onAdd,
    onRemove,
    onBypass,
  }: {
    chain: readonly ChainEntry[];
    /** The device whose panel is shown. */
    shownId: number | null;
    /** The effects that can be added. */
    effects: readonly DeviceDescriptor[];
    onSelect: (device: Device) => void;
    onAdd: (descriptor: DeviceDescriptor) => void;
    onRemove: (device: Device) => void;
    onBypass: (device: Device) => void;
  } = $props();
</script>

<section class="chain" aria-label="FX">
  <h3>FX</h3>
  <ul>
    {#each chain as entry (entry.device.id)}
      {@const device = entry.device}
      <li class:shown={device.id === shownId} class:bypassed={!device.enabled}>
        <button
          type="button"
          class="name"
          aria-pressed={device.id === shownId}
          title="Show {device.name}"
          onclick={() => onSelect(device)}
        >
          {device.name}
        </button>
        {#if entry.role === "insert"}
          <button
            type="button"
            class="bypass"
            class:on={!device.enabled}
            aria-pressed={!device.enabled}
            aria-label="Bypass {device.name}"
            title="Bypass"
            onclick={() => onBypass(device)}
          >
            B
          </button>
          <button
            type="button"
            class="remove"
            aria-label="Remove {device.name}"
            title="Remove"
            onclick={() => onRemove(device)}
          >
            &times;
          </button>
        {/if}
      </li>
    {/each}
  </ul>
  <select
    class="add"
    aria-label="Add effect"
    value=""
    disabled={effects.length === 0}
    onchange={(event) => {
      const kind = event.currentTarget.value;
      event.currentTarget.value = "";
      const descriptor = effects.find((d) => d.kind === kind);
      if (descriptor) onAdd(descriptor);
    }}
  >
    <option value="" disabled>Add effect&hellip;</option>
    {#each effects as effect (effect.kind)}
      <option value={effect.kind}>{effect.name}</option>
    {/each}
  </select>
</section>

<style>
  .chain {
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 6px;
    width: 180px;
    flex: none;
    overflow-y: auto;
    padding: 6px 10px;
    border-left: 1px solid var(--border);
    background: var(--panel);
  }
  h3 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  ul {
    display: flex;
    flex-direction: column;
    gap: 3px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  li {
    display: flex;
    align-items: center;
    gap: 3px;
    border-radius: 3px;
  }
  li.shown {
    background: var(--panel-2);
    outline: 1px solid var(--accent);
  }
  li.bypassed .name {
    color: var(--muted);
    text-decoration: line-through;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-align: left;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .bypass,
  .remove {
    padding: 1px 6px;
  }
  .bypass.on {
    background: var(--accent);
    color: var(--bg);
  }
</style>
