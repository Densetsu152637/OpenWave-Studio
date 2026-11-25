<script lang="ts">
  import { noteBars } from "../arrange/midi";
  import { peakRequest, waveformColumns } from "../arrange/waveform";
  import { beatToX, clipVisibleSpan, type View } from "../arrange/view";
  import type { TempoMap } from "../arrange/time";
  import { peaksRange, tauriTransport, type PeakBins } from "../ipc";
  import type { AdjustMode } from "../arrange/clips/edit";
  import type { TakeBadge } from "../record/takes";
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
    adjusting = false,
    onpointerdown,
    ondblclick,
    onhandle,
    badge = null,
    onTake,
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
    /** A trim or fade drag is running: keep the peaks drawn and fetch again when it lands. */
    adjusting?: boolean;
    onpointerdown: (event: PointerEvent) => void;
    /** Double-click: opens a MIDI clip in the piano roll. */
    ondblclick?: (event: MouseEvent) => void;
    /** Pointer down on a trim edge or fade handle. */
    onhandle: (event: PointerEvent, mode: AdjustMode) => void;
    /** The take badge when the clip is the active take of a group with other takes. */
    badge?: TakeBadge | null;
    /** Chooses the take with this index (a take of the clip's group). */
    onTake?: (index: number) => void;
  } = $props();

  /** Pause between asking again for peaks that are still being built. */
  const RETRY_MS = 500;
  const MAX_RETRIES = 20;

  let canvas = $state<HTMLCanvasElement>();
  let peaks = $state.raw<PeakBins | null>(null);

  // Plain numbers, so scrolling (a new `view` object each frame) only re-runs what really changed.
  const pxPerBeat = $derived(view.pxPerBeat);
  const left = $derived(beatToX(view, displayStart));
  const boxWidth = $derived(clip.length * pxPerBeat);
  // Fades (audio clips) in pixels; plain numbers, so a scroll changes nothing here.
  const fadeInPx = $derived(clip.content.type === "audio" ? clip.content.fade_in * pxPerBeat : 0);
  const fadeOutPx = $derived(clip.content.type === "audio" ? clip.content.fade_out * pxPerBeat : 0);
  const gainDb = $derived(clip.content.type === "audio" ? clip.content.gain_db : 0);
  const span = $derived(clipVisibleSpan(view, clip, width));
  const spanLeft = $derived(span ? span.left : 0);
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
    // Each pointer move changes the request; the old peaks stay until the edit lands.
    if (adjusting) return;
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
      const left = spanLeft;
      for (const bar of noteBars(clip.content.notes, pxPerBeat, height)) {
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
      style:left="{spanLeft}px"
      style:width="{drawWidth}px"
      style:height="{height}px"
    ></canvas>
  {/if}
  {#if clip.content.type === "audio"}
    {#if fadeInPx > 0}
      <svg class="ramp in" style:width="{fadeInPx}px" viewBox="0 0 1 1" preserveAspectRatio="none" aria-hidden="true">
        <polygon points="0,0 1,0 0,1" />
      </svg>
    {/if}
    {#if fadeOutPx > 0}
      <svg class="ramp out" style:width="{fadeOutPx}px" viewBox="0 0 1 1" preserveAspectRatio="none" aria-hidden="true">
        <polygon points="0,0 1,0 1,1" />
      </svg>
    {/if}
    <div
      class="fade-handle"
      style:left="{fadeInPx}px"
      title="Fade in (drag)"
      role="presentation"
      onpointerdown={(event) => onhandle(event, "fade-in")}
    ></div>
    <div
      class="fade-handle"
      style:right="{fadeOutPx}px"
      title="Fade out (drag)"
      role="presentation"
      onpointerdown={(event) => onhandle(event, "fade-out")}
    ></div>
  {/if}
  <div class="edge left" title="Trim start (drag)" role="presentation" onpointerdown={(event) => onhandle(event, "trim-left")}></div>
  <div class="edge right" title="Trim end (drag)" role="presentation" onpointerdown={(event) => onhandle(event, "trim-right")}></div>
  <span class="name">{clip.name}{missing ? " (media missing)" : ""}{clip.muted ? " (muted)" : ""}</span>
  {#if badge && onTake}
    <select
      class="take"
      aria-label="Take of {clip.name}"
      title="Choose which take plays"
      onpointerdown={(event) => event.stopPropagation()}
      onchange={(event) => onTake(Number(event.currentTarget.value))}
    >
      {#each badge.choices as choice (choice.index)}
        <option value={choice.index} selected={choice.active}>
          {choice.active ? (boxWidth < 96 ? `${badge.position}/${badge.count}` : badge.label) : choice.label}
        </option>
      {/each}
    </select>
  {/if}
  {#if gainDb !== 0}<span class="gain">{gainDb > 0 ? "+" : ""}{gainDb.toFixed(1)} dB</span>{/if}
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
  .edge {
    position: absolute;
    top: 0;
    bottom: 0;
    width: min(6px, 30%);
    cursor: ew-resize;
  }
  .edge.left {
    left: 0;
  }
  .edge.right {
    right: 0;
  }
  .edge:hover {
    background: color-mix(in srgb, var(--track) 55%, transparent);
  }
  .ramp {
    position: absolute;
    top: 0;
    height: 100%;
    fill: color-mix(in srgb, var(--bg) 70%, transparent);
    pointer-events: none;
  }
  .ramp.in {
    left: 0;
  }
  .ramp.out {
    right: 0;
  }
  .fade-handle {
    position: absolute;
    top: 0;
    width: 8px;
    height: 8px;
    margin-left: -4px;
    box-sizing: border-box;
    background: var(--text);
    border: 1px solid var(--track);
    border-radius: 2px;
    cursor: col-resize;
    opacity: 0.75;
    z-index: 1;
  }
  .fade-handle[style*="right"] {
    margin-left: 0;
    margin-right: -4px;
  }
  .fade-handle:hover {
    opacity: 1;
  }
  .take {
    position: absolute;
    bottom: 1px;
    left: 2px;
    max-width: calc(100% - 4px);
    padding: 0 2px;
    font-size: 10px;
    line-height: 14px;
    z-index: 1;
    cursor: pointer;
  }
  .gain {
    position: absolute;
    bottom: 1px;
    right: 8px;
    font-size: 10px;
    color: var(--text);
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
