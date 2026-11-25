<script lang="ts">
  import { cssColor } from "../arrange/view";
  import type { Bus, Output, StripSettings } from "../ipc/types";
  import ColorPicker from "./ColorPicker.svelte";
  import StripMixer from "./StripMixer.svelte";

  /**
   * The header of a bus or of the master strip: the same controls as a track header (fader, pan,
   * mute, output) without a lane. A bus can be renamed and deleted; the master cannot. Selecting
   * it shows its FX chain (and, for a bus, its sends) in the dock.
   */
  let {
    name,
    color,
    master,
    settings,
    selected,
    height,
    destinations,
    onSelect,
    onRename,
    onMute,
    onVolume,
    onPan,
    onOutput,
    onDelete,
    onColor,
    canMoveUp = false,
    canMoveDown = false,
    onMoveUp,
    onMoveDown,
  }: {
    name: string;
    color: number;
    /** The master strip: no rename, output or delete. */
    master: boolean;
    settings: StripSettings;
    selected: boolean;
    height: number;
    destinations: readonly Bus[];
    onSelect: () => void;
    onRename: (name: string) => void;
    onMute: (mute: boolean) => void;
    onVolume: (gainDb: number) => void;
    onPan: (pan: number) => void;
    onOutput: (output: Output) => void;
    onDelete: () => void;
    /** Recolor and reorder: buses only (the master has none of these). */
    onColor?: (color: number) => void;
    canMoveUp?: boolean;
    canMoveDown?: boolean;
    onMoveUp?: () => void;
    onMoveDown?: () => void;
  } = $props();

  let draft = $state<string | null>(null);
  const shown = $derived(draft ?? name);

  function commitName() {
    if (draft !== null) onRename(draft);
    draft = null;
  }

  function onNameKey(event: KeyboardEvent) {
    if (event.key === "Enter") (event.currentTarget as HTMLInputElement).blur();
    else if (event.key === "Escape") {
      draft = null;
      (event.currentTarget as HTMLInputElement).blur();
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="header"
  class:selected
  class:master
  style:height="{height}px"
  style:--track={cssColor(color)}
  onpointerdown={onSelect}
>
  <div class="row">
    {#if master}
      <span class="name fixed">Master</span>
    {:else}
      {#if onColor}<ColorPicker {color} label="Bus color" onPick={onColor} />{/if}
      <input
        class="name"
        type="text"
        aria-label="Bus name"
        value={shown}
        maxlength="256"
        oninput={(event) => (draft = event.currentTarget.value)}
        onblur={commitName}
        onkeydown={onNameKey}
      />
    {/if}
    <button
      type="button"
      class="mute"
      class:on={settings.mute}
      aria-pressed={settings.mute}
      title="Mute"
      onclick={() => onMute(!settings.mute)}
    >
      M
    </button>
    {#if !master && onMoveUp && onMoveDown}
      <button type="button" class="move" title="Move bus up" aria-label="Move bus up" disabled={!canMoveUp} onclick={onMoveUp}>
        &#9650;
      </button>
      <button type="button" class="move" title="Move bus down" aria-label="Move bus down" disabled={!canMoveDown} onclick={onMoveDown}>
        &#9660;
      </button>
    {/if}
    {#if !master}
      <button type="button" class="delete" title="Delete bus" aria-label="Delete bus" onclick={onDelete}>
        &times;
      </button>
    {/if}
  </div>
  <StripMixer {settings} {destinations} routable={!master} {onVolume} {onPan} {onOutput} />
</div>

<style>
  .header {
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 4px;
    padding: 4px 6px;
    border-bottom: 1px solid var(--border);
    border-left: 4px solid var(--track);
    background: var(--panel);
  }
  .header.master {
    --track: var(--muted);
  }
  .header.selected {
    background: var(--panel-2);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .name {
    flex: 1;
    min-width: 0;
    font: inherit;
    color: var(--text);
    background: transparent;
    border: 1px solid transparent;
    border-radius: 3px;
    padding: 1px 3px;
    user-select: text;
  }
  .name.fixed {
    font-weight: 600;
    user-select: none;
  }
  input.name:focus {
    border-color: var(--accent);
    background: var(--bg);
    outline: none;
  }
  .mute,
  .delete {
    padding: 1px 6px;
  }
  .mute.on {
    background: var(--accent);
    color: var(--bg);
  }
  .move {
    padding: 1px 3px;
    font-size: 9px;
  }
</style>
