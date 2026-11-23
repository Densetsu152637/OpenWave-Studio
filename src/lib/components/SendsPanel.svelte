<script lang="ts">
  import { MAX_SEND_DB, MIN_SEND_DB, dbLabel } from "../mixer/mixer";
  import type { AuxSend, Bus } from "../ipc/types";

  /**
   * The sends of the strip shown in the dock: for each, its bus, level, a pre- or post-fader
   * toggle and a remove button; the menu adds a send to a bus. It edits nothing itself.
   */
  let {
    sends,
    buses,
    targets,
    onAdd,
    onLevel,
    onPre,
    onRemove,
  }: {
    sends: readonly AuxSend[];
    /** Every bus, to name a send's target. */
    buses: readonly Bus[];
    /** The buses a send can be added to (no routing cycle). */
    targets: readonly Bus[];
    onAdd: (bus: number) => void;
    /** Called once the slider is released. */
    onLevel: (index: number, levelDb: number) => void;
    onPre: (index: number, pre: boolean) => void;
    onRemove: (index: number) => void;
  } = $props();

  let sliding = $state<{ index: number; value: number } | null>(null);
  const nameOf = (id: number) => buses.find((b) => b.id === id)?.name ?? `Bus ${id}`;
</script>

<section class="sends" aria-label="Sends">
  <h3>Sends</h3>
  <ul>
    {#each sends as send, index (index)}
      {@const level = sliding?.index === index ? sliding.value : send.level_db}
      <li>
        <span class="target" title={nameOf(send.target)}>{nameOf(send.target)}</span>
        <button
          type="button"
          class="pre"
          class:on={send.pre_fader}
          aria-pressed={send.pre_fader}
          title={send.pre_fader ? "Pre-fader (click for post-fader)" : "Post-fader (click for pre-fader)"}
          onclick={() => onPre(index, !send.pre_fader)}
        >
          {send.pre_fader ? "Pre" : "Post"}
        </button>
        <button
          type="button"
          class="remove"
          aria-label="Remove send to {nameOf(send.target)}"
          title="Remove"
          onclick={() => onRemove(index)}
        >
          &times;
        </button>
        <input
          class="level"
          type="range"
          aria-label="Send level to {nameOf(send.target)}"
          min={MIN_SEND_DB}
          max={MAX_SEND_DB}
          step="0.1"
          value={level}
          oninput={(event) => (sliding = { index, value: event.currentTarget.valueAsNumber })}
          onchange={(event) => {
            sliding = null;
            onLevel(index, event.currentTarget.valueAsNumber);
          }}
        />
        <span class="db">{dbLabel(level, MIN_SEND_DB)}</span>
      </li>
    {/each}
  </ul>
  <select
    class="add"
    aria-label="Add send"
    value=""
    disabled={targets.length === 0}
    onchange={(event) => {
      const id = Number(event.currentTarget.value);
      event.currentTarget.value = "";
      if (Number.isInteger(id)) onAdd(id);
    }}
  >
    <option value="" disabled>{targets.length === 0 ? "No buses" : "Add send…"}</option>
    {#each targets as bus (bus.id)}
      <option value={String(bus.id)}>{bus.name}</option>
    {/each}
  </select>
</section>

<style>
  .sends {
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 6px;
    width: 200px;
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
    gap: 6px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  li {
    display: grid;
    grid-template-columns: 1fr auto auto;
    gap: 3px 4px;
    align-items: center;
  }
  .target {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pre,
  .remove {
    padding: 1px 6px;
  }
  .pre.on {
    background: var(--accent);
    color: var(--bg);
  }
  .level {
    grid-column: 1 / 3;
    min-width: 0;
  }
  .db {
    text-align: right;
    color: var(--muted);
  }
</style>
