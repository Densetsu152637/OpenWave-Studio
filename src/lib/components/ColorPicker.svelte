<script lang="ts">
  import { TRACK_COLORS } from "../arrange/edits";
  import { cssColor } from "../arrange/view";

  /** A swatch button that opens a small palette; picking a color calls `onPick` and closes it. */
  let {
    color,
    label,
    onPick,
  }: {
    color: number;
    /** Accessible name, such as "Track color". */
    label: string;
    onPick: (color: number) => void;
  } = $props();

  let open = $state(false);
  let root = $state<HTMLElement>();

  function outside(event: PointerEvent) {
    if (open && root && !root.contains(event.target as Node)) open = false;
  }

  function keydown(event: KeyboardEvent) {
    if (open && event.key === "Escape") {
      event.stopPropagation();
      open = false;
    }
  }
</script>

<svelte:window onpointerdown={outside} onkeydown={keydown} />

<span class="picker" bind:this={root}>
  <button
    type="button"
    class="swatch"
    style:background={cssColor(color)}
    aria-label={label}
    aria-expanded={open}
    title={label}
    onclick={() => (open = !open)}
  ></button>
  {#if open}
    <span class="palette" role="menu" aria-label={label}>
      {#each TRACK_COLORS as option (option)}
        <button
          type="button"
          role="menuitem"
          class="swatch"
          class:current={option === color}
          style:background={cssColor(option)}
          aria-label={cssColor(option)}
          onclick={() => {
            open = false;
            onPick(option);
          }}
        ></button>
      {/each}
    </span>
  {/if}
</span>

<style>
  .picker {
    position: relative;
    display: inline-flex;
  }
  .swatch {
    width: 14px;
    height: 14px;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 3px;
  }
  .swatch.current {
    outline: 2px solid var(--accent);
  }
  .palette {
    position: absolute;
    top: 18px;
    left: 0;
    z-index: 10;
    display: flex;
    gap: 4px;
    padding: 4px;
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-radius: 4px;
  }
</style>
