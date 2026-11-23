<script lang="ts">
  import { onMount } from "svelte";
  import Lane from "./Lane.svelte";
  import PianoRoll from "./PianoRoll.svelte";
  import TrackHeader from "./TrackHeader.svelte";
  import DeviceChain from "./DeviceChain.svelte";
  import DevicePanel from "../devices/DevicePanel.svelte";
  import {
    addInsert,
    chainOf,
    effectDescriptors,
    removeInsertCommand,
    shownEntry,
    toggleBypassCommand,
  } from "../devices/chain";
  import { loadDescriptors } from "../devices/descriptors";
  import { descriptorFor, setParamCommand } from "../devices/params";
  import { replaceNotesCommand } from "../pianoroll/notes";
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
  import { addMidiTrack, createMidiClip } from "../arrange/midi";
  import { armedSet, recordApi, toggleArm } from "../arrange/record";
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
  import type {
    Clip,
    Device,
    DeviceDescriptor,
    Note,
    ParamDescriptorDto,
    Track,
  } from "../ipc/types";

  let {
    mirror,
    playheadBeat,
    recording,
    onLocate,
    onError,
  }: {
    mirror: MirrorState;
    /** A recording is running: arming is locked. */
    recording: boolean;
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
  /** The MIDI clip open in the piano roll. */
  let openClip = $state<number | null>(null);
  /** The chain device whose panel is shown; the first one is shown when it is gone. */
  let selectedDevice = $state<number | null>(null);
  let descriptors = $state.raw<readonly DeviceDescriptor[]>([]);

  const project = $derived(mirror.project);
  const tracks = $derived(project?.tracks ?? []);
  const tempo = $derived(new TempoMap(project?.tempo_map ?? []));
  const signatures = $derived(new SignatureMap(project?.time_signature_map ?? []));
  const marks = $derived(rulerMarks(view, width, signatures));
  const missing = $derived(new Set(mirror.history?.missingMedia ?? []));
  const armed = $derived(armedSet(mirror.history));
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
  // The piano roll follows the project too: it closes when its clip is gone (undo, delete, open).
  const rollClip = $derived.by(() => {
    const clip = openClip === null ? undefined : project?.clips.get(openClip);
    return clip?.content.type === "midi" ? clip : undefined;
  });
  const rollTrack = $derived(rollClip ? tracks.find((t) => t.id === rollClip.track) : undefined);
  /** The track whose FX list and panel the dock shows: the open clip's, else the selected one. */
  const dockTrack = $derived(rollTrack ?? tracks.find((t) => t.id === selectedTrack));
  const chain = $derived(dockTrack && project ? chainOf(dockTrack, project.devices) : []);
  const dockDevice = $derived(shownEntry(chain, selectedDevice)?.device);
  const dockDescriptor = $derived(dockDevice ? descriptorFor(descriptors, dockDevice) : undefined);
  const effects = $derived(effectDescriptors(descriptors));
  const selectedMidiTrack = $derived(
    tracks.find((t) => t.id === selectedTrack && t.kind === "midi"),
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

  const addInstrumentTrack = () =>
    run(async () => {
      selectedTrack = await addMidiTrack(commandsApi, tracks.length, descriptors);
      selectedClip = null;
    });

  const newMidiClip = () =>
    run(async () => {
      const track = selectedMidiTrack;
      if (!track) return;
      const count = clipsByTrack.get(track.id)?.length ?? 0;
      selectedClip = await createMidiClip(commandsApi, signatures, track, playheadBeat, count);
    });

  const editNotes = (clip: Clip, notes: Note[]) =>
    run(() => commandsApi.edit(replaceNotesCommand(clip, notes)));

  const setParam = (device: { id: number }, param: ParamDescriptorDto, value: number) =>
    run(() => commandsApi.edit(setParamCommand(device, param, value)));

  const addEffect = (track: Track, descriptor: DeviceDescriptor) =>
    run(async () => {
      selectedDevice = await addInsert(commandsApi, track, descriptor);
    });

  const removeEffect = (device: Device) => run(() => commandsApi.edit(removeInsertCommand(device)));

  const bypassEffect = (device: Device) => run(() => commandsApi.edit(toggleBypassCommand(device)));

  function openRoll(clip: Clip) {
    if (clip.content.type !== "midi") return;
    if (selectedTrack !== clip.track) selectedDevice = null;
    selectedTrack = clip.track;
    selectedClip = clip.id;
    openClip = clip.id;
  }

  onMount(() => {
    loadDescriptors()
      .then((loaded) => (descriptors = loaded))
      .catch(() => {});
  });

  function selectClip(track: Track, id: number) {
    if (selectedTrack !== track.id) selectedDevice = null;
    selectedTrack = track.id;
    selectedClip = id;
  }

  function selectTrack(id: number) {
    if (selectedTrack !== id) selectedDevice = null;
    selectedTrack = id;
    selectedClip = null;
  }
</script>

<svelte:window onkeydown={onKey} />

<main class="arrangement" aria-label="Arrangement">
  <div class="toolbar">
    <button type="button" onclick={addAudioTrack} disabled={!project}>Add audio track</button>
    <button type="button" onclick={addInstrumentTrack} disabled={!project || descriptors.length === 0}>
      Add MIDI track
    </button>
    <button type="button" onclick={newMidiClip} disabled={!selectedMidiTrack}>New MIDI clip</button>
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
                armed={armed.has(track.id)}
                armLocked={recording}
                onArm={() => run(() => toggleArm(recordApi, armed, track.id))}
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
                onOpenClip={openRoll}
              />
            </div>
          </div>
        {/each}

        {#if tracks.length === 0}
          <p class="empty">Add an audio or MIDI track, or import an audio file, to begin.</p>
        {/if}

        {#if playheadX >= 0 && playheadX <= width}
          <div class="playhead" style:left="{HEADER_PX + playheadX}px" aria-hidden="true"></div>
        {/if}
      </div>
    </div>
    {#if dockTrack}
      <section class="dock" aria-label="Piano roll, FX and device panel">
        {#if rollClip && rollTrack}
          <PianoRoll
            clip={rollClip}
            color={rollTrack.color}
            {signatures}
            onEdit={(notes) => editNotes(rollClip, notes)}
            onClose={() => (openClip = null)}
          />
        {:else}
          <p class="dock-hint">
            {dockTrack.kind === "midi"
              ? "Double-click a MIDI clip to edit its notes."
              : "Add effects to the track from the FX list."}
          </p>
        {/if}
        <DeviceChain
          {chain}
          shownId={dockDevice?.id ?? null}
          {effects}
          onSelect={(device) => (selectedDevice = device.id)}
          onAdd={(descriptor) => addEffect(dockTrack, descriptor)}
          onRemove={removeEffect}
          onBypass={bypassEffect}
        />
        {#if dockDevice && dockDescriptor}
          <DevicePanel
            device={dockDevice}
            descriptor={dockDescriptor}
            onParam={(param, value) => setParam(dockDevice, param, value)}
          />
        {/if}
      </section>
    {/if}
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
  .dock {
    display: flex;
    height: 280px;
    flex: none;
    border-top: 1px solid var(--border);
    background: var(--bg);
  }
  .dock-hint {
    flex: 1;
    margin: 16px;
    color: var(--muted);
  }
  .empty {
    margin: 24px;
    color: var(--muted);
  }
</style>
