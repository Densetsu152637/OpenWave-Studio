<script lang="ts">
  import { tick } from "svelte";
  import type { SignatureMap } from "../arrange/time";
  import { cssColor } from "../arrange/view";
  import type { Clip, Note } from "../ipc/types";
  import { type Drag, dragPreview, draggedId, press } from "../pianoroll/gesture";
  import {
    DEFAULT_GRID,
    DEFAULT_ROLL_VIEW,
    GRID_OPTIONS,
    KEYS,
    type RollView,
    gridLines,
    hitTest,
    initialScrollTop,
    isBlackKey,
    keyName,
    noteRect,
    rollSize,
    zoomRoll,
  } from "../pianoroll/grid";
  import { clipNotes, noteId, removeNote } from "../pianoroll/notes";

  /**
   * The piano roll of one MIDI clip: click to draw a note, drag a note to move it, drag its right
   * edge to resize it, select it and press Delete (or right-click it) to remove it. Every
   * finished gesture is one `onEdit` with the clip's new note list; the clip itself only changes
   * when the resulting patch arrives.
   */
  let {
    clip,
    color,
    signatures,
    onEdit,
    onClose,
  }: {
    clip: Clip;
    /** The track's color (0xRRGGBB). */
    color: number;
    signatures: SignatureMap;
    onEdit: (notes: Note[]) => void;
    onClose: () => void;
  } = $props();

  const ZOOM_STEP = 1.25;

  let view = $state<RollView>({ ...DEFAULT_ROLL_VIEW });
  let grid = $state(DEFAULT_GRID);
  let selected = $state<string | null>(null);
  let drag = $state<{ drag: Drag; preview: Note[] | null } | null>(null);
  let scroller = $state<HTMLElement>();
  let root = $state<HTMLElement>();
  /** The last press was inside the roll, so Delete means "delete the selected note". */
  let active = $state(false);
  let surface = $state<HTMLElement>();

  const committed = $derived(clipNotes(clip));
  const notes = $derived(drag ? (drag.preview ?? drag.drag.notes) : committed);
  const size = $derived(rollSize(view, clip.length));
  const lines = $derived(gridLines(view, clip.start, clip.length, grid, signatures));
  const keys = Array.from({ length: KEYS }, (_, row) => KEYS - 1 - row);

  // Scroll the notes' key range into view when a clip opens (not on every edit).
  let shownClip: number | null = null;
  $effect(() => {
    const id = clip.id;
    const target = scroller;
    if (!target || shownClip === id) return;
    shownClip = id;
    selected = null;
    void tick().then(() => {
      target.scrollTop = initialScrollTop(committed, view, target.clientHeight);
    });
  });

  function local(event: PointerEvent) {
    const rect = surface!.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  }

  function onPointerDown(event: PointerEvent) {
    if (event.button !== 0 || !surface) return;
    const { x, y } = local(event);
    const result = press(committed, view, x, y, grid, clip.length);
    if (result.type === "none") {
      selected = null;
      return;
    }
    selected = result.selected;
    if (result.type === "add") {
      onEdit(result.notes);
      return;
    }
    surface.setPointerCapture(event.pointerId);
    drag = { drag: result.drag, preview: null };
  }

  function onPointerMove(event: PointerEvent) {
    if (!drag) return;
    const { x, y } = local(event);
    const preview = dragPreview(drag.drag, view, x, y, grid, clip.length);
    drag = { drag: drag.drag, preview };
    selected = draggedId(drag.drag, preview);
  }

  function onPointerUp(event: PointerEvent) {
    if (!drag) return;
    const done = drag;
    drag = null;
    if (event.type === "pointerup" && done.preview) onEdit(done.preview);
  }

  function onContextMenu(event: MouseEvent) {
    event.preventDefault();
    if (!surface) return;
    const rect = surface.getBoundingClientRect();
    const hit = hitTest(committed, view, event.clientX - rect.left, event.clientY - rect.top);
    if (hit) {
      selected = null;
      onEdit(removeNote(committed, hit.index));
    }
  }

  function onKey(event: KeyboardEvent) {
    const target = event.target as HTMLElement | null;
    if (target?.closest("input, textarea, select, [contenteditable]")) return;
    if (active && (event.key === "Delete" || event.key === "Backspace") && selected !== null) {
      const index = committed.findIndex((n) => noteId(n) === selected);
      if (index >= 0) {
        event.preventDefault();
        event.stopPropagation();
        selected = null;
        onEdit(removeNote(committed, index));
      }
    }
  }
</script>

<svelte:window
  onkeydowncapture={onKey}
  onpointerdowncapture={(event) => (active = root?.contains(event.target as Node) ?? false)}
/>

<section class="roll" aria-label="Piano roll" style:--track={cssColor(color)} bind:this={root}>
  <header>
    <strong>{clip.name}</strong>
    <label>
      Grid
      <select bind:value={grid} aria-label="Snap grid">
        {#each GRID_OPTIONS as option (option.beats)}
          <option value={option.beats}>{option.label}</option>
        {/each}
      </select>
    </label>
    <span class="spacer"></span>
    <button type="button" aria-label="Zoom out" onclick={() => (view = zoomRoll(view, 1 / ZOOM_STEP))}>&minus;</button>
    <button type="button" aria-label="Zoom in" onclick={() => (view = zoomRoll(view, ZOOM_STEP))}>+</button>
    <button type="button" onclick={onClose}>Close</button>
  </header>

  <div class="scroll" bind:this={scroller}>
    <div class="inner" style:width="{size.width + 48}px" style:height="{size.height}px">
      <div class="keys" aria-hidden="true">
        {#each keys as key (key)}
          <div
            class="key"
            class:black={isBlackKey(key)}
            style:height="{view.pxPerKey}px"
            style:line-height="{view.pxPerKey}px"
          >
            {key % 12 === 0 ? keyName(key) : ""}
          </div>
        {/each}
      </div>
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="surface"
        bind:this={surface}
        style:width="{size.width}px"
        style:height="{size.height}px"
        onpointerdown={onPointerDown}
        onpointermove={onPointerMove}
        onpointerup={onPointerUp}
        onpointercancel={onPointerUp}
        oncontextmenu={onContextMenu}
      >
        {#each keys as key (key)}
          <div
            class="row"
            class:black={isBlackKey(key)}
            style:top="{(KEYS - 1 - key) * view.pxPerKey}px"
            style:height="{view.pxPerKey}px"
          ></div>
        {/each}
        {#each lines as line (line.beat)}
          <div class="line {line.kind}" style:left="{line.x}px"></div>
        {/each}
        {#each notes as note (noteId(note))}
          {@const rect = noteRect(view, note)}
          <div
            class="note"
            class:selected={selected === noteId(note)}
            style:left="{rect.x}px"
            style:top="{rect.y}px"
            style:width="{rect.width}px"
            style:height="{rect.height}px"
            role="presentation"
          ></div>
        {/each}
      </div>
    </div>
  </div>
</section>

<style>
  .roll {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    min-height: 0;
    background: var(--bg);
  }
  header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 3px 8px;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
    font-size: 12px;
  }
  .spacer {
    flex: 1;
  }
  .scroll {
    flex: 1;
    min-height: 0;
    overflow: auto;
  }
  .inner {
    position: relative;
    display: flex;
  }
  .keys {
    position: sticky;
    left: 0;
    z-index: 2;
    flex: none;
    width: 48px;
    background: var(--panel);
    border-right: 1px solid var(--border);
  }
  .key {
    box-sizing: border-box;
    padding-left: 4px;
    font-size: 9px;
    color: var(--muted);
    border-bottom: 1px solid var(--border);
    background: var(--panel);
  }
  .key.black {
    background: var(--panel-2);
  }
  .surface {
    position: relative;
    flex: none;
    cursor: crosshair;
    touch-action: none;
    user-select: none;
  }
  .row {
    position: absolute;
    left: 0;
    right: 0;
    box-sizing: border-box;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 50%, transparent);
    pointer-events: none;
  }
  .row.black {
    background: color-mix(in srgb, var(--panel-2) 60%, transparent);
  }
  .line {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 0;
    border-left: 1px solid color-mix(in srgb, var(--border) 45%, transparent);
    pointer-events: none;
  }
  .line.beat {
    border-left-color: color-mix(in srgb, var(--border) 85%, transparent);
  }
  .line.bar {
    border-left-color: var(--muted);
  }
  .note {
    position: absolute;
    box-sizing: border-box;
    border: 1px solid color-mix(in srgb, var(--track) 70%, black);
    border-radius: 2px;
    background: var(--track);
    cursor: grab;
  }
  .note.selected {
    outline: 2px solid var(--accent);
    outline-offset: -1px;
  }
</style>
