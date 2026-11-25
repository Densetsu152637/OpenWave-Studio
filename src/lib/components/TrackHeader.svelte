<script lang="ts">
  import { cssColor } from "../arrange/view";
  import type { Bus, Output, Track } from "../ipc/types";
  import ColorPicker from "./ColorPicker.svelte";
  import StripMixer from "./StripMixer.svelte";

  let {
    track,
    selected,
    height,
    armed,
    armLocked,
    onSelect,
    onRename,
    onMute,
    onSolo,
    onArm,
    onVolume,
    onPan,
    onOutput,
    destinations,
    onDelete,
    onColor,
    canMoveUp,
    canMoveDown,
    onMoveUp,
    onMoveDown,
  }: {
    track: Track;
    selected: boolean;
    height: number;
    /** The track is armed for recording. */
    armed: boolean;
    /** Arming cannot change now (a recording is running). */
    armLocked: boolean;
    onSelect: () => void;
    onRename: (name: string) => void;
    onMute: (mute: boolean) => void;
    onSolo: (solo: boolean) => void;
    onArm: () => void;
    /** Called once the slider is released, with the new gain in dB. */
    onVolume: (gainDb: number) => void;
    /** Called once the slider is released, with the new pan (-1 left to 1 right). */
    onPan: (pan: number) => void;
    onOutput: (output: Output) => void;
    /** The buses the output may be set to. */
    destinations: readonly Bus[];
    onDelete: () => void;
    onColor: (color: number) => void;
    /** The track is not first / last, so it can move up / down. */
    canMoveUp: boolean;
    canMoveDown: boolean;
    onMoveUp: () => void;
    onMoveDown: () => void;
  } = $props();

  let draft = $state<string | null>(null);

  const name = $derived(draft ?? track.name);

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
    <ColorPicker color={track.color} label="Track color" onPick={onColor} />
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
    <span class="move-col">
      <button type="button" class="move" title="Move track up" aria-label="Move track up" disabled={!canMoveUp} onclick={onMoveUp}>
        &#9650;
      </button>
      <button type="button" class="move" title="Move track down" aria-label="Move track down" disabled={!canMoveDown} onclick={onMoveDown}>
        &#9660;
      </button>
    </span>
    {#if track.kind === "audio"}
      <button
        type="button"
        class="arm"
        class:on={armed}
        aria-pressed={armed}
        disabled={armLocked}
        title="Arm for recording"
        aria-label="Arm for recording"
        onclick={onArm}
      >
        R
      </button>
    {/if}
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
    <button
      type="button"
      class="solo"
      class:on={track.strip.settings.solo}
      aria-pressed={track.strip.settings.solo}
      title="Solo"
      onclick={() => onSolo(!track.strip.settings.solo)}
    >
      S
    </button>
    <button type="button" class="delete" title="Delete track" aria-label="Delete track" onclick={onDelete}>
      &times;
    </button>
  </div>
  <StripMixer settings={track.strip.settings} {destinations} routable={true} {onVolume} {onPan} {onOutput} />
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
  .solo,
  .arm,
  .delete {
    padding: 1px 6px;
  }
  .move-col {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .move {
    padding: 0 3px;
    font-size: 7px;
    line-height: 9px;
  }
  .mute,
  .solo,
  .arm,
  .delete {
    padding: 1px 4px;
  }
  .arm.on {
    background: var(--danger);
    color: var(--bg);
  }
  .mute.on,
  .solo.on {
    background: var(--accent);
    color: var(--bg);
  }
</style>
