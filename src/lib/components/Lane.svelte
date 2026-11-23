<script lang="ts">
  import ClipView from "./ClipView.svelte";
  import type { SignatureMap, TempoMap } from "../arrange/time";
  import { cssColor, dragClipStart, type View } from "../arrange/view";
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
    signatures,
    selectedClip,
    onSelectTrack,
    onSelectClip,
    onMoveClip,
  }: {
    track: Track;
    clips: readonly Clip[];
    media: ReadonlyMap<number, MediaItem>;
    missing: ReadonlySet<number>;
    view: View;
    width: number;
    height: number;
    tempo: TempoMap;
    signatures: SignatureMap;
    selectedClip: number | null;
    onSelectTrack: () => void;
    onSelectClip: (id: number) => void;
    onMoveClip: (clip: Clip, start: number) => void;
  } = $props();

  /** Pointer travel below which a press is a click, not a drag. */
  const DRAG_THRESHOLD_PX = 3;

  let drag = $state<{ id: number; startX: number; start: number; moved: boolean } | null>(null);
  let dragTo = $state(0);

  function press(event: PointerEvent, clip: Clip) {
    if (event.button !== 0) return;
    event.stopPropagation();
    onSelectClip(clip.id);
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    drag = { id: clip.id, startX: event.clientX, start: clip.start, moved: false };
    dragTo = clip.start;
  }

  function move(event: PointerEvent) {
    if (!drag) return;
    const dx = event.clientX - drag.startX;
    if (!drag.moved && Math.abs(dx) < DRAG_THRESHOLD_PX) return;
    drag.moved = true;
    dragTo = dragClipStart(drag.start, dx, view, signatures);
  }

  function release(event: PointerEvent) {
    if (!drag) return;
    const done = drag;
    drag = null;
    const clip = clips.find((c) => c.id === done.id);
    if (done.moved && clip && event.type === "pointerup") onMoveClip(clip, dragTo);
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
  {#each clips as clip (clip.id)}
    <ClipView
      {clip}
      media={clip.content.type === "audio" ? media.get(clip.content.media) : undefined}
      color={cssColor(track.color)}
      {view}
      {width}
      {height}
      {tempo}
      selected={selectedClip === clip.id}
      missing={clip.content.type === "audio" && missing.has(clip.content.media)}
      displayStart={drag?.moved && drag.id === clip.id ? dragTo : clip.start}
      onpointerdown={(event) => press(event, clip)}
    />
  {/each}
</div>

<style>
  .lane {
    position: relative;
    overflow: hidden;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
  }
</style>
