<script lang="ts">
  import { noteBars } from "../arrange/midi";
  import { peakRequest, waveformColumns } from "../arrange/waveform";
  import { beatToX, clipVisibleSpan, type View } from "../arrange/view";
  import type { TempoMap } from "../arrange/time";
  import { peaksRange, tauriTransport, type PeakBins } from "../ipc";
  import type { Clip, MediaItem } from "../ipc/types";

  let {
    clip,
    media,
    color,
    view,
    width,
    height,
    tempo,
    selected,
    missing,
    displayStart,
    onpointerdown,
    ondblclick,
  }: {
    clip: Clip;
    media: MediaItem | undefined;
    color: string;
    view: View;
    /** Width of the lane in pixels. */
    width: number;
    height: number;
    tempo: TempoMap;
    selected: boolean;
    missing: boolean;
    /** Where the clip is shown while it is dragged, in beats. */
    displayStart: number;
    onpointerdown: (event: PointerEvent) => void;
    /** Double-click: opens a MIDI clip in the piano roll. */
    ondblclick?: (event: MouseEvent) => void;
  } = $props();

  /** Pause between asking again for peaks that are still being built. */
  const RETRY_MS = 500;
  const MAX_RETRIES = 20;

  let canvas = $state<HTMLCanvasElement>();
  let peaks = $state.raw<PeakBins | null>(null);

  const left = $derived(beatToX(view, displayStart));
  const boxWidth = $derived(clip.length * view.pxPerBeat);
  const span = $derived(clipVisibleSpan(view, clip, width));
  const drawWidth = $derived(span ? Math.ceil(span.right - span.left) : 0);
  const request = $derived(
    media && span && !missing
      ? peakRequest(clip, media, tempo, boxWidth, span.left, span.right)
      : null,
  );
  // A string, so an unchanged request does not fetch again.
  const requestKey = $derived(
    request ? `${request.media}:${request.start}:${request.end}:${request.bins}` : null,
  );

  $effect(() => {
    const key = requestKey;
    if (key === null || !request || !tauriTransport.available) {
      peaks = null;
      return;
    }
    const { media: id, start, end, bins } = request;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let attempts = 0;
    const load = async () => {
      try {
        const result = await peaksRange(id, start, end, bins);
        if (cancelled) return;
        // No bins: the peak file is not built yet; ask again shortly.
        if (result.bins === 0 && attempts++ < MAX_RETRIES) {
          timer = setTimeout(load, RETRY_MS);
          return;
        }
        peaks = result;
      } catch {
        if (!cancelled && attempts++ < 3) timer = setTimeout(load, RETRY_MS);
      }
    };
    void load();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  });

  $effect(() => {
    const target = canvas;
    if (!target) return;
    const ratio = window.devicePixelRatio || 1;
    target.width = Math.max(1, Math.round(drawWidth * ratio));
    target.height = Math.max(1, Math.round(height * ratio));
    const context = target.getContext("2d");
    if (!context) return;
    context.clearRect(0, 0, target.width, target.height);
    if (clip.content.type === "midi") {
      // A MIDI clip shows its notes as plain bars; the part of the clip left of the window is
      // not drawn, so bars shift by the window's left edge.
      context.fillStyle = getComputedStyle(target).color;
      const left = span?.left ?? 0;
      for (const bar of noteBars(clip.content.notes, view.pxPerBeat, height)) {
        context.fillRect(
          (bar.x - left) * ratio,
          bar.y * ratio,
          bar.width * ratio,
          bar.height * ratio,
        );
      }
      return;
    }
    if (!peaks || peaks.bins === 0) return;
    const columns = waveformColumns(peaks.data, target.width);
    context.fillStyle = getComputedStyle(target).color;
    const mid = target.height / 2;
    for (let c = 0; c < target.width; c++) {
      const top = mid - columns[2 * c + 1]! * mid;
      const bottom = mid - columns[2 * c]! * mid;
      context.fillRect(c, top, 1, Math.max(1, bottom - top));
    }
  });
</script>

<div
  class="clip"
  class:selected
  class:missing
  class:muted={clip.muted}
  style:left="{left}px"
  style:width="{boxWidth}px"
  style:--track={color}
  role="button"
  tabindex="-1"
  aria-label="Clip {clip.name}"
  aria-pressed={selected}
  {onpointerdown}
  {ondblclick}
>
  {#if span}
    <canvas
      bind:this={canvas}
      style:left="{span.left}px"
      style:width="{drawWidth}px"
      style:height="{height}px"
    ></canvas>
  {/if}
  <span class="name">{clip.name}{missing ? " (media missing)" : ""}</span>
</div>

<style>
  .clip {
    position: absolute;
    top: 2px;
    bottom: 2px;
    box-sizing: border-box;
    overflow: hidden;
    border: 1px solid var(--track);
    border-radius: 3px;
    background: color-mix(in srgb, var(--track) 22%, var(--panel));
    color: color-mix(in srgb, var(--track) 85%, var(--text));
    cursor: grab;
    touch-action: none;
  }
  .clip.selected {
    outline: 2px solid var(--accent);
    outline-offset: -1px;
  }
  .clip.muted {
    opacity: 0.45;
  }
  .clip.missing {
    border-style: dashed;
    border-color: var(--danger);
    background: var(--panel-2);
  }
  canvas {
    position: absolute;
    top: 0;
    pointer-events: none;
  }
  .name {
    position: absolute;
    top: 1px;
    left: 4px;
    right: 4px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: 11px;
    color: var(--text);
    pointer-events: none;
  }
</style>
