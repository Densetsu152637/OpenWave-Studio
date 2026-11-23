<script lang="ts">
  import Lane from "./Lane.svelte";
  import TrackHeader from "./TrackHeader.svelte";
  import { nativeDialogs } from "../arrange/dialogs";
  import {
    addTrack,
    commandsApi,
    deleteClip,
    importAudio,
    moveClip,
    removeTrack,
    renameTrack,
    setTrackStrip,
  } from "../arrange/edits";
  import { SignatureMap, TempoMap } from "../arrange/time";
  import {
    DEFAULT_VIEW,
    beatToX,
    rulerMarks,
    scrollBy,
    xToBeat,
    zoomAt,
    type View,
  } from "../arrange/view";
  import type { MirrorState } from "../stores";
  import type { Clip, Track } from "../ipc/types";

  let {
    mirror,
    playheadBeat,
    onLocate,
    onError,
  }: {
    mirror: MirrorState;
    /** Playhead position in beats. */
    playheadBeat: number;
    onLocate: (beat: number) => void;
    onError: (message: string) => void;
  } = $props();

  const HEADER_PX = 200;
  const LANE_PX = 72;
  const RULER_PX = 24;
  const ZOOM_STEP = 1.25;
  const WHEEL_ZOOM = 0.002;

  let view = $state.raw<View>(DEFAULT_VIEW);
  let width = $state(800);
  let selectedTrack = $state<number | null>(null);
  let selectedClip = $state<number | null>(null);

  const project = $derived(mirror.project);
  const tracks = $derived(project?.tracks ?? []);
  const tempo = $derived(new TempoMap(project?.tempo_map ?? []));
  const signatures = $derived(new SignatureMap(project?.time_signature_map ?? []));
  const marks = $derived(rulerMarks(view, width, signatures));
  const missing = $derived(new Set(mirror.history?.missingMedia ?? []));
  const clipsByTrack = $derived.by(() => {
    const byTrack = new Map<number, Clip[]>();
    for (const clip of project?.clips.values() ?? []) {
      const list = byTrack.get(clip.track);
      if (list) list.push(clip);
      else byTrack.set(clip.track, [clip]);
    }
    return byTrack;
  });
  const playheadX = $derived(beatToX(view, playheadBeat));
  // The selection follows the project: a track or clip that is gone (undo, open) is not selected.
  const trackSelected = (id: number) => selectedTrack === id && tracks.some((t) => t.id === id);
  const clipSelected = $derived(
    selectedClip !== null && (project?.clips.has(selectedClip) ?? false) ? selectedClip : null,
  );

  /** Runs an async action, reporting a failure instead of leaving it unhandled. */
  async function run(action: () => Promise<unknown>) {
    try {
      await action();
    } catch (error) {
      onError(String(error));
    }
  }

  const zoomBy = (factor: number) => (view = zoomAt(view, factor, width / 2));

  function onWheel(event: WheelEvent) {
    if (event.ctrlKey || event.metaKey) {
      event.preventDefault();
      const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
      view = zoomAt(view, Math.exp(-event.deltaY * WHEEL_ZOOM), event.clientX - rect.left);
    } else if (event.shiftKey || Math.abs(event.deltaX) > Math.abs(event.deltaY)) {
      event.preventDefault();
      view = scrollBy(view, event.deltaX || event.deltaY);
    }
  }

  function onRulerDown(event: PointerEvent) {
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    onLocate(Math.max(0, xToBeat(view, event.clientX - rect.left)));
  }

  function onKey(event: KeyboardEvent) {
    const target = event.target as HTMLElement | null;
    if (target?.closest("input, textarea, select, [contenteditable]")) return;
    if ((event.key === "Delete" || event.key === "Backspace") && clipSelected !== null) {
      const clip = project?.clips.get(clipSelected);
      if (clip) {
        event.preventDefault();
        void run(() => deleteClip(commandsApi, clip));
      }
    }
  }

  const addAudioTrack = () =>
    run(async () => {
      selectedTrack = await addTrack(commandsApi, tracks.length);
    });

  const importFile = () =>
    run(async () => {
      const path = await nativeDialogs.pickAudioToImport();
      if (!path) return;
      const target = selectedTrack !== null && tracks.some((t) => t.id === selectedTrack);
      const result = await importAudio(commandsApi, tempo, path, {
        trackId: target ? selectedTrack : null,
        trackCount: tracks.length,
        start: playheadBeat,
      });
      selectedTrack = result.trackId;
      selectedClip = result.clipId;
    });

  function selectClip(track: Track, id: number) {
    selectedTrack = track.id;
    selectedClip = id;
  }

  function selectTrack(id: number) {
    selectedTrack = id;
    selectedClip = null;
  }
</script>

<svelte:window onkeydown={onKey} />

<main class="arrangement" aria-label="Arrangement">
  <div class="toolbar">
    <button type="button" onclick={addAudioTrack} disabled={!project}>Add audio track</button>
    <button type="button" onclick={importFile} disabled={!project}>Import audio&hellip;</button>
    <span class="spacer"></span>
    <button type="button" aria-label="Zoom out" title="Zoom out" onclick={() => zoomBy(1 / ZOOM_STEP)}>&minus;</button>
    <button type="button" aria-label="Zoom in" title="Zoom in" onclick={() => zoomBy(ZOOM_STEP)}>+</button>
  </div>

  {#if !project}
    <p class="empty">No project loaded.</p>
  {:else}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="scroller" onwheel={onWheel}>
      <div class="content">
        <div class="row ruler-row" style:height="{RULER_PX}px">
          <div class="corner" style:width="{HEADER_PX}px"></div>
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="ruler" bind:clientWidth={width} onpointerdown={onRulerDown}>
            {#each marks as mark (mark.kind + mark.beat)}
              <div class="mark {mark.kind}" style:left="{mark.x}px">
                {#if mark.label}<span>{mark.label}</span>{/if}
              </div>
            {/each}
          </div>
        </div>

        {#each tracks as track (track.id)}
          <div class="row">
            <div class="head" style:width="{HEADER_PX}px">
              <TrackHeader
                {track}
                height={LANE_PX}
                selected={trackSelected(track.id)}
                onSelect={() => selectTrack(track.id)}
                onRename={(name) => run(() => renameTrack(commandsApi, track, name))}
                onMute={(mute) => run(() => setTrackStrip(commandsApi, track, { mute }))}
                onVolume={(gain_db) => run(() => setTrackStrip(commandsApi, track, { gain_db }))}
                onDelete={() => run(() => removeTrack(commandsApi, track.id))}
              />
            </div>
            <div class="lane-cell">
              <Lane
                {track}
                clips={clipsByTrack.get(track.id) ?? []}
                media={project.media_pool}
                {missing}
                {view}
                {width}
                height={LANE_PX}
                {tempo}
                {signatures}
                selectedClip={clipSelected}
                onSelectTrack={() => selectTrack(track.id)}
                onSelectClip={(id) => selectClip(track, id)}
                onMoveClip={(clip, start) => run(() => moveClip(commandsApi, clip, start))}
              />
            </div>
          </div>
        {/each}

        {#if tracks.length === 0}
          <p class="empty">Add an audio track or import an audio file to begin.</p>
        {/if}

        {#if playheadX >= 0 && playheadX <= width}
          <div class="playhead" style:left="{HEADER_PX + playheadX}px" aria-hidden="true"></div>
        {/if}
      </div>
    </div>
  {/if}
</main>

<style>
  .arrangement {
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--bg);
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 8px;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
  }
  .spacer {
    flex: 1;
  }
  .scroller {
    flex: 1;
    min-height: 0;
    overflow-x: hidden;
    overflow-y: auto;
  }
  .content {
    position: relative;
    min-height: 100%;
  }
  .row {
    display: flex;
  }
  .head {
    flex: none;
  }
  .lane-cell {
    flex: 1;
    min-width: 0;
  }
  .ruler-row {
    position: sticky;
    top: 0;
    z-index: 2;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
  }
  .corner {
    flex: none;
    border-right: 1px solid var(--border);
  }
  .ruler {
    position: relative;
    flex: 1;
    min-width: 0;
    overflow: hidden;
    cursor: pointer;
  }
  .mark {
    position: absolute;
    top: 0;
    bottom: 0;
    border-left: 1px solid var(--border);
    pointer-events: none;
  }
  .mark.beat {
    top: 70%;
  }
  .mark span {
    position: absolute;
    left: 3px;
    top: 2px;
    font-size: 11px;
    color: var(--muted);
  }
  .playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 0;
    border-left: 1px solid var(--accent);
    z-index: 3;
    pointer-events: none;
  }
  .empty {
    margin: 24px;
    color: var(--muted);
  }
</style>
