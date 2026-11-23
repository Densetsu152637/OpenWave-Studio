<script lang="ts">
  import { cssColor } from "../arrange/view";
  import type { Track } from "../ipc/types";

  const MIN_DB = -60;
  const MAX_DB = 6;

  let {
    track,
    selected,
    height,
    onSelect,
    onRename,
    onMute,
    onVolume,
    onDelete,
  }: {
    track: Track;
    selected: boolean;
    height: number;
    onSelect: () => void;
    onRename: (name: string) => void;
    onMute: (mute: boolean) => void;
    /** Called once the slider is released, with the new gain in dB. */
    onVolume: (gainDb: number) => void;
    onDelete: () => void;
  } = $props();

  let draft = $state<string | null>(null);
  let sliding = $state<number | null>(null);

  const name = $derived(draft ?? track.name);
  const gain = $derived(sliding ?? track.strip.settings.gain_db);

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
  style:height="{height}px"
  style:--track={cssColor(track.color)}
  onpointerdown={onSelect}
>
  <div class="row">
    <input
      class="name"
      type="text"
      aria-label="Track name"
      value={name}
      maxlength="256"
      oninput={(event) => (draft = event.currentTarget.value)}
      onblur={commitName}
      onkeydown={onNameKey}
    />
    <button
      type="button"
      class="mute"
      class:on={track.strip.settings.mute}
      aria-pressed={track.strip.settings.mute}
      title="Mute"
      onclick={() => onMute(!track.strip.settings.mute)}
    >
      M
    </button>
    <button type="button" class="delete" title="Delete track" aria-label="Delete track" onclick={onDelete}>
      &times;
    </button>
  </div>
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
    <span class="db">{gain <= MIN_DB ? "-inf" : gain.toFixed(1)} dB</span>
  </div>
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
  .name:focus {
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
  .volume {
    flex: 1;
    min-width: 0;
  }
  .db {
    width: 58px;
    text-align: right;
    color: var(--muted);
  }
</style>
