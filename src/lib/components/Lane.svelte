<script lang="ts">
  import { onDestroy } from "svelte";
  import ClipView from "./ClipView.svelte";
  import type { DragMode } from "../arrange/clips/edit";
  import type { ClipSelection } from "../arrange/selection";
  import type { TempoMap } from "../arrange/time";
  import { cssColor, type View } from "../arrange/view";
  import type { TakeBadge } from "../record/takes";
  import type { Clip, MediaItem, Track } from "../ipc/types";

  let {
    track,
    clips,
    media,
    missing,
    view,
    width,
    height,
    tempo,
    selection,
    moving,
    preview,
    onSelectTrack,
    onPress,
    onRelease,
    onDrag,
    onDragEnd,
    onOpenClip,
    badgeOf,
    onTake,
  }: {
    track: Track;
    clips: readonly Clip[];
    media: ReadonlyMap<number, MediaItem>;
    missing: ReadonlySet<number>;
    view: View;
    width: number;
    height: number;
    tempo: TempoMap;
    selection: ClipSelection;
    /** Where clips are shown while they are dragged (and until the move lands), by id. */
    moving: ReadonlyMap<number, number> | null;
    /** A clip shown trimmed or faded while its handle is dragged (and until the edit lands). */
    preview: Clip | null;
    onSelectTrack: () => void;
    /** Pointer down on a clip; `additive` is Shift or Ctrl/Cmd. */
    onPress: (clip: Clip, additive: boolean) => void;
    /** Pointer up after a press, whether or not it dragged. */
    onRelease: (clip: Clip, additive: boolean, moved: boolean) => void;
    /**
     * The pointer travelled `dx` pixels from the press; `alt` bypasses the snap grid. `mode` is
     * "move" for the clip body, else the trim edge or fade handle that was grabbed.
     */
    onDrag: (clip: Clip, dx: number, alt: boolean, mode: DragMode) => void;
    /** The drag ended; `commit` is false when it was cancelled. */
    onDragEnd: (commit: boolean) => void;
    /** Double-click on a clip. */
    onOpenClip: (clip: Clip) => void;
    /** The take badge of a clip, null when it has none. */
    badgeOf: (clip: Clip) => TakeBadge | null;
    /** Chooses the take with `index` in the group of `clip`. */
    onTake: (clip: Clip, index: number) => void;
  } = $props();

  /** Pointer travel below which a press is a click, not a drag. */
  const DRAG_THRESHOLD_PX = 3;

  let drag: {
    id: number;
    startX: number;
    moved: boolean;
    additive: boolean;
    mode: DragMode;
  } | null = null;

  // A lane scrolled out of the rendered rows mid-drag loses its pointer capture: cancel the drag.
  onDestroy(() => {
    if (drag?.moved) onDragEnd(false);
    drag = null;
  });

  function press(event: PointerEvent, clip: Clip, mode: DragMode = "move") {
    if (event.button !== 0) return;
    event.stopPropagation();
    const additive = event.shiftKey || event.ctrlKey || event.metaKey;
    onPress(clip, additive);
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    drag = { id: clip.id, startX: event.clientX, moved: false, additive, mode };
  }

  function move(event: PointerEvent) {
    if (!drag) return;
    const dx = event.clientX - drag.startX;
    if (!drag.moved && Math.abs(dx) < DRAG_THRESHOLD_PX) return;
    const clip = clips.find((c) => c.id === drag?.id);
    if (!clip) return;
    drag.moved = true;
    onDrag(clip, dx, event.altKey, drag.mode);
  }

  function release(event: PointerEvent) {
    if (!drag) return;
    const done = drag;
    drag = null;
    const clip = clips.find((c) => c.id === done.id);
    if (clip) onRelease(clip, done.additive, done.moved);
    if (done.moved) onDragEnd(event.type === "pointerup");
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="lane"
  style:height="{height}px"
  onpointerdown={onSelectTrack}
  onpointermove={move}
  onpointerup={release}
  onpointercancel={release}
>
  {#each clips as stored (stored.id)}
    {@const clip = preview?.id === stored.id ? preview : stored}
    <ClipView
      {clip}
      adjusting={preview?.id === stored.id}
      media={clip.content.type === "audio" ? media.get(clip.content.media) : undefined}
      color={cssColor(track.color)}
      {view}
      {width}
      {height}
      {tempo}
      selected={selection.has(clip.id)}
      missing={clip.content.type === "audio" && missing.has(clip.content.media)}
      displayStart={moving?.get(clip.id) ?? clip.start}
      onpointerdown={(event) => press(event, stored)}
      onhandle={(event, mode) => press(event, stored, mode)}
      ondblclick={() => onOpenClip(stored)}
      badge={badgeOf(stored)}
      onTake={(index) => onTake(stored, index)}
    />
  {/each}
</div>

<style>
  .lane {
    box-sizing: border-box;
    position: relative;
    overflow: hidden;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
  }
</style>
