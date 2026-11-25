<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { tauriTransport } from "../ipc";
  import Lane from "./Lane.svelte";
  import PianoRoll from "./PianoRoll.svelte";
  import TrackHeader from "./TrackHeader.svelte";
  import BusHeader from "./BusHeader.svelte";
  import SendsPanel from "./SendsPanel.svelte";
  import DeviceChain from "./DeviceChain.svelte";
  import DevicePanel from "../devices/DevicePanel.svelte";
  import {
    addStripInsert,
    chainOf,
    effectDescriptors,
    removeInsertCommand,
    shownEntry,
    stripChainOf,
    toggleBypassCommand,
  } from "../devices/chain";
  import {
    addBus,
    changeSend,
    moveBus,
    removeBus,
    renameBus,
    routeTargets,
    setBusColor,
    sameStrip,
    setSendsCommand,
    setStripCommand,
    stripOf,
    withSend,
    withoutSend,
  } from "../mixer/mixer";
  import {
    deviceLabel,
    instrumentDescriptors,
    loadDescriptors,
    pluginDescriptors,
    resetDescriptors,
    watchPlugins,
  } from "../devices/descriptors";
  import { descriptorFor, setParamCommand } from "../devices/params";
  import { pluginEditorOpen } from "../ipc/commands";
  import { replaceNotesCommand } from "../pianoroll/notes";
  import { nativeDialogs } from "../arrange/dialogs";
  import {
    addTrack,
    commandsApi,
    importAudio,
    moveClips,
    moveTrack,
    removeTrack,
    renameTrack,
    reorderTarget,
    setTrackColor,
    setTrackStrip,
  } from "../arrange/edits";
  import {
    adjustClip,
    commitAdjust,
    dragTarget,
    duplicateClips,
    splittable,
    setClipsGain,
    splitClips,
    toggleClipsMuted,
    MAX_GAIN_DB,
    MIN_GAIN_DB,
    type DragMode,
  } from "../arrange/clips/edit";
  import { dropTarget, importAudioFiles } from "../arrange/clips/drop";
  import { arrangeKeyAction } from "../arrange/keys";
  import { addMarker, removeMarker, renameMarker } from "../arrange/markers";
  import { timeMarks } from "../arrange/ruler";
  import {
    NO_SELECTION,
    pressClip,
    pruneSelection,
    releaseClip,
    selectAll,
    type ClipSelection,
  } from "../arrange/selection";
  import {
    DEFAULT_SNAP,
    SNAP_OPTIONS,
    dragDelta,
    isSnapResolution,
    snapBeat,
    type SnapResolution,
  } from "../arrange/snap";
  import { installPerfHook } from "../perf";
  import { pingPong } from "../perf/scroll";
  import { addMidiTrack, createMidiClip } from "../arrange/midi";
  import { armedSet, recordApi, toggleArm } from "../arrange/record";
  import {
    badgeFor,
    chooseTake,
    commitRange,
    deleteClipsKeepingTakes,
    dragRange,
    grabAt,
    laneClips,
    setTrackInputCommand,
    setTrackMonitorCommand,
    takeGroups,
    type RangeDrag,
    type RangeKind,
  } from "../record";
  import { SignatureMap, TempoMap } from "../arrange/time";
  import {
    DEFAULT_VIEW,
    cssColor,
    beatToX,
    projectEndBeat,
    rulerMarks,
    visibleClips,
    visibleRows,
    scrollBy,
    xToBeat,
    zoomAt,
    type View,
  } from "../arrange/view";
  import type { MirrorState } from "../stores";
  import type { Strip } from "../ipc/types";
  import type {
    BeatRange,
    Bus,
    Clip,
    Device,
    DeviceDescriptor,
    Marker,
    MixerRef,
    Note,
    StripSettings,
    ParamDescriptorDto,
    Track,
  } from "../ipc/types";

  let {
    mirror,
    playheadBeat,
    recording,
    inputChannels = null,
    onLocate,
    onError,
  }: {
    /** Channels of the input device the tracks record from; null without one. */
    inputChannels?: number | null;
    mirror: MirrorState;
    /** A recording is running: arming is locked. */
    recording: boolean;
    /** Playhead position in beats. */
    playheadBeat: number;
    onLocate: (beat: number) => void;
    onError: (message: string) => void;
  } = $props();

  const HEADER_PX = 200;
  const LANE_PX = 108;
  const RULER_PX = 24;
  const MARKER_PX = 20;
  /** The punch strip under the markers. */
  const PUNCH_PX = 14;
  /** The bottom part of the ruler that holds the loop bar. */
  const LOOP_BAR_PX = 10;
  /** Height above the first track lane. */
  const TOP_PX = RULER_PX + MARKER_PX + PUNCH_PX;
  const ZOOM_STEP = 1.25;
  const WHEEL_ZOOM = 0.002;

  let view = $state.raw<View>(DEFAULT_VIEW);
  let width = $state(800);
  let selectedTrack = $state<number | null>(null);
  /** A bus (its id) or the master selected instead of a track; its FX and sends fill the dock. */
  let selectedBus = $state<number | "master" | null>(null);
  let selection = $state.raw<ClipSelection>(NO_SELECTION);
  /** Beats the selected clips are shown moved by while they are dragged. */
  let moving = $state.raw<ReadonlyMap<number, number> | null>(null);
  /** The clip shown trimmed or faded while a handle is dragged, and until the edit lands. */
  let preview = $state.raw<Clip | null>(null);
  /** The clip body or handle the drag running now grabbed. */
  let dragMode: DragMode = "move";
  /** The pointer travel's beat shift of the drag running now. */
  let dragShift = 0;
  let dragging = false;
  /** Scroll position and height of the track area, for rendering only the tracks in view. */
  let scrollTop = $state(0);
  let scrollHeight = $state(600);
  let snap = $state<SnapResolution>(DEFAULT_SNAP);
  let rulerMode = $state<"bars" | "time">("bars");
  /** The marker being renamed, and the name typed so far. */
  let editingMarker = $state<number | null>(null);
  let markerDraft = $state("");
  let scroller = $state<HTMLElement>();
  /** The MIDI clip open in the piano roll. */
  let openClip = $state<number | null>(null);
  /** The chain device whose panel is shown; the first one is shown when it is gone. */
  let selectedDevice = $state<number | null>(null);
  let descriptors = $state.raw<readonly DeviceDescriptor[]>([]);

  const project = $derived(mirror.project);
  const tracks = $derived(project?.tracks ?? []);
  const buses = $derived(project?.buses ?? []);
  const tempo = $derived(new TempoMap(project?.tempo_map ?? []));
  const signatures = $derived(new SignatureMap(project?.time_signature_map ?? []));
  const trackIds = $derived(tracks.map((t) => t.id));
  const busIds = $derived(buses.map((b) => b.id));
  // Only the tracks in (or near) the viewport are rendered; spacers keep the scroll height.
  const firstRow = $derived(
    visibleRows(scrollTop - TOP_PX - 2, scrollHeight, LANE_PX, tracks.length).first,
  );
  const lastRow = $derived(
    visibleRows(scrollTop - TOP_PX - 2, scrollHeight, LANE_PX, tracks.length).last,
  );
  const visibleTracks = $derived(tracks.slice(firstRow, lastRow));
  const marks = $derived(rulerMarks(view, width, signatures));
  const clockMarks = $derived(rulerMode === "time" ? timeMarks(view, width, tempo) : []);
  const markers = $derived(
    [...(project?.markers.values() ?? [])].sort((a, b) => a.beat - b.beat || a.id - b.id),
  );
  const shownMarkers = $derived(
    markers.filter((m) => {
      const x = beatToX(view, m.beat);
      return x >= -1 && x <= width;
    }),
  );
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
  // Takes of one recording stack: the lane draws each group's active take, whose badge reaches
  // the others.
  const takes = $derived(takeGroups(project?.clips.values() ?? []));
  const laneByTrack = $derived.by(() => {
    const byTrack = new Map<number, Clip[]>();
    for (const [track, list] of clipsByTrack) byTrack.set(track, laneClips(list));
    return byTrack;
  });
  const settings = $derived(project?.settings);
  /** A range gesture running now: what it grabbed, and the range it shows until it lands. */
  let rangeGesture = $state.raw<{
    kind: RangeKind;
    drag: RangeDrag;
    shown: BeatRange | null;
    /** Pointer x at the press; a press that stays within a few pixels is a click. */
    startX: number;
  } | null>(null);
  const shownRange = (kind: RangeKind): BeatRange | null =>
    rangeGesture?.kind === kind && rangeGesture.shown
      ? rangeGesture.shown
      : ((kind === "loop" ? settings?.loop_range : settings?.punch_range) ?? null);
  const loopRange = $derived(shownRange("loop"));
  const punchRange = $derived(shownRange("punch"));
  const playheadX = $derived(beatToX(view, playheadBeat));
  // The selection follows the project: a track or clip that is gone (undo, open) is not selected.
  const trackSelected = (id: number) => selectedTrack === id && tracks.some((t) => t.id === id);
  const liveSelection = $derived(
    pruneSelection(selection, (id) => project?.clips.has(id) ?? false),
  );
  const selectedClips = $derived(
    [...liveSelection].flatMap((id) => {
      const clip = project?.clips.get(id);
      return clip ? [clip] : [];
    }),
  );
  // The piano roll follows the project too: it closes when its clip is gone (undo, delete, open).
  const rollClip = $derived.by(() => {
    const clip = openClip === null ? undefined : project?.clips.get(openClip);
    return clip?.content.type === "midi" ? clip : undefined;
  });
  const rollTrack = $derived(rollClip ? tracks.find((t) => t.id === rollClip.track) : undefined);
  /** The track whose FX list and panel the dock shows: the open clip's, else the selected one. */
  const dockTrack = $derived(rollTrack ?? tracks.find((t) => t.id === selectedTrack));
  /** The bus or master the dock shows when no track is selected. */
  const dockBus = $derived<MixerRef | null>(
    dockTrack || !project
      ? null
      : selectedBus === "master"
        ? "master"
        : selectedBus !== null && buses.some((b) => b.id === selectedBus)
          ? { bus: selectedBus }
          : null,
  );
  /** What the dock is about: a track, a bus or the master. */
  const dockRef = $derived<MixerRef | null>(dockTrack ? { track: dockTrack.id } : dockBus);
  const dockStrip = $derived(project && dockRef ? stripOf(project, dockRef) : undefined);
  const chain = $derived(
    !project || !dockStrip
      ? []
      : dockTrack
        ? chainOf(dockTrack, project.devices)
        : stripChainOf(dockStrip, project.devices),
  );
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

  /** The range the loop or punch gesture would edit, or null when none is stored. */
  const storedRange = (kind: RangeKind): BeatRange | null =>
    (kind === "loop" ? settings?.loop_range : settings?.punch_range) ?? null;

  function startRange(kind: RangeKind, event: PointerEvent, grab: RangeDrag["grab"]) {
    const target = event.currentTarget as HTMLElement;
    const rect = target.getBoundingClientRect();
    const range = storedRange(kind);
    const anchor = xToBeat(view, event.clientX - rect.left);
    event.preventDefault();
    target.setPointerCapture(event.pointerId);
    rangeGesture = { kind, drag: { grab, origin: range, anchor }, shown: null, startX: event.clientX };
  }

  function moveRange(event: PointerEvent) {
    if (!rangeGesture) return;
    if (!rangeGesture.shown && Math.abs(event.clientX - rangeGesture.startX) < 3) return;
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const beat = xToBeat(view, event.clientX - rect.left);
    rangeGesture = {
      ...rangeGesture,
      shown: dragRange(rangeGesture.drag, beat, { signatures, snap, bypass: event.altKey }),
    };
  }

  function endRange(event: PointerEvent) {
    const gesture = rangeGesture;
    rangeGesture = null;
    if (!gesture || !settings || event.type === "pointercancel" || !gesture.shown) return;
    // One undo step for the whole gesture.
    void run(() => commitRange(commandsApi, settings, gesture.kind, gesture.shown));
  }

  const clearRange = (kind: RangeKind) =>
    settings && storedRange(kind) && run(() => commitRange(commandsApi, settings, kind, null));

  /** Grabs the stored range's end or body under the pointer, or a new range when over none. */
  function grabFor(kind: RangeKind, event: PointerEvent): RangeDrag["grab"] {
    const range = storedRange(kind);
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    return grabAt(
      range,
      event.clientX - rect.left,
      range ? beatToX(view, range.start) : 0,
      range ? beatToX(view, range.end) : 0,
    );
  }

  /**
   * Click on the ruler places the playhead. Shift+drag makes a new loop range; dragging the
   * ends or body of the loop bar (the lower strip of the ruler) edits it.
   */
  function onRulerDown(event: PointerEvent) {
    if (event.button !== 0) return;
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    if (settings) {
      if (event.shiftKey) return startRange("loop", event, "create");
      if (event.clientY - rect.top >= RULER_PX - LOOP_BAR_PX) {
        const grab = grabFor("loop", event);
        if (grab !== "create") return startRange("loop", event, grab);
      }
    }
    onLocate(Math.max(0, xToBeat(view, event.clientX - rect.left)));
  }

  /** The punch strip: drag on empty space for a new range, on the bar to move or resize it. */
  function onPunchDown(event: PointerEvent) {
    if (event.button !== 0 || !settings) return;
    startRange("punch", event, grabFor("punch", event));
  }

  function onKey(event: KeyboardEvent) {
    const target = event.target as HTMLElement | null;
    if (target?.closest("input, textarea, select, [contenteditable]")) return;
    const action = arrangeKeyAction(event, { rollOpen: rollClip !== undefined });
    if (!action || !project) return;
    switch (action) {
      case "locate-start":
        event.preventDefault();
        onLocate(0);
        break;
      case "locate-end":
        event.preventDefault();
        onLocate(projectEndBeat(project.clips.values()));
        break;
      case "zoom-in":
        event.preventDefault();
        zoomBy(ZOOM_STEP);
        break;
      case "zoom-out":
        event.preventDefault();
        zoomBy(1 / ZOOM_STEP);
        break;
      case "add-marker":
        event.preventDefault();
        void addMarkerAtPlayhead(false);
        break;
      case "select-all":
        event.preventDefault();
        selection = selectAll(project.clips.keys());
        break;
      case "deselect":
        if (liveSelection.size > 0) selection = NO_SELECTION;
        break;
      case "split":
        event.preventDefault();
        splitAtPlayhead();
        break;
      case "duplicate":
        event.preventDefault();
        duplicateSelection();
        break;
      case "toggle-mute":
        event.preventDefault();
        void muteSelection();
        break;
      case "delete-selection":
        if (selectedClips.length > 0) {
          event.preventDefault();
          const doomed = selectedClips;
          selection = NO_SELECTION;
          void run(() => deleteClipsKeepingTakes(commandsApi, project.clips.values(), doomed));
        }
        break;
    }
  }

  /** Adds a marker at the playhead, on the snap grid unless `bypass` (Alt) is held. */
  const addMarkerAtPlayhead = (bypass: boolean) =>
    run(() => addMarker(commandsApi, snapBeat(playheadBeat, snap, signatures, bypass), markers));

  function startMarkerRename(marker: Marker) {
    editingMarker = marker.id;
    markerDraft = marker.name;
  }

  function finishMarkerRename(marker: Marker, commit: boolean) {
    if (editingMarker !== marker.id) return;
    editingMarker = null;
    if (commit) void run(() => renameMarker(commandsApi, marker, markerDraft));
  }

  function markerKey(event: KeyboardEvent, marker: Marker) {
    if (event.key === "Enter") finishMarkerRename(marker, true);
    else if (event.key === "Escape") finishMarkerRename(marker, false);
  }

  function focusOnMount(node: HTMLInputElement) {
    node.focus();
    node.select();
  }

  /** Moves a track or bus one place up (-1) or down (1) its list: one undo step. */
  const moveBy = (
    ids: readonly number[],
    id: number,
    by: number,
    move: (api: typeof commandsApi, id: number, to: number) => Promise<unknown>,
  ) =>
    run(async () => {
      const to = reorderTarget(ids, id, by);
      if (to !== null) await move(commandsApi, id, to);
    });

  function onPressClip(track: Track, clip: Clip, additive: boolean) {
    if (selectedTrack !== track.id) selectedDevice = null;
    selectedTrack = track.id;
    selectedBus = null;
    selection = pressClip(liveSelection, clip.id, additive);
  }

  function onReleaseClip(clip: Clip, additive: boolean, moved: boolean) {
    selection = releaseClip(liveSelection, clip.id, additive, moved);
  }

  function onDragClip(clip: Clip, dx: number, alt: boolean, mode: DragMode) {
    dragMode = mode;
    if (mode !== "move") {
      dragging = true;
      const target = dragTarget(clip, mode, dx, view, signatures, snap, alt);
      preview = adjustClip(clip, mode, target, tempo, mediaOf(clip));
      return;
    }
    const minStart = Math.min(...selectedClips.map((c) => c.start));
    dragging = true;
    dragShift = dragDelta(clip.start, minStart, dx, view, signatures, snap, alt);
    moving = new Map(selectedClips.map((c) => [c.id, c.start + dragShift]));
  }

  function onDragEnd(commit: boolean) {
    dragging = false;
    if (dragMode !== "move") {
      const adjusted = preview;
      const mode = dragMode;
      dragMode = "move";
      const original = adjusted ? project?.clips.get(adjusted.id) : undefined;
      if (!commit || !adjusted || !original) {
        preview = null;
        return;
      }
      void run(() => commitAdjust(commandsApi, original, adjusted, mode)).finally(
        () => (preview = null),
      );
      return;
    }
    const delta = dragShift;
    dragShift = 0;
    if (!commit || delta === 0) {
      moving = null;
      return;
    }
    // The clips stay where they were dropped until the move has landed, so they never jump back.
    void run(() => moveClips(commandsApi, selectedClips, delta)).finally(() => (moving = null));
  }

  const mediaOf = (clip: Clip) =>
    clip.content.type === "audio" ? project?.media_pool.get(clip.content.media) : undefined;
  const splitTargets = $derived(splittable(selectedClips, playheadBeat, tempo, mediaOf));
  const selectedAudio = $derived(selectedClips.filter((c) => c.content.type === "audio"));
  /** The gain every selected audio clip shares, or null when they differ (or none is selected). */
  const sharedGain = $derived.by(() => {
    const first = selectedAudio[0]?.content;
    if (first?.type !== "audio") return null;
    return selectedAudio.every(
      (c) => c.content.type === "audio" && c.content.gain_db === first.gain_db,
    )
      ? first.gain_db
      : null;
  });

  function splitAtPlayhead() {
    if (splitTargets.length === 0) return;
    void run(async () => {
      const created = await splitClips(commandsApi, selectedClips, playheadBeat, tempo, mediaOf);
      selection = new Set([...liveSelection, ...created]);
    });
  }

  function duplicateSelection() {
    if (selectedClips.length === 0) return;
    void run(async () => {
      selection = new Set(await duplicateClips(commandsApi, selectedClips));
    });
  }

  const muteSelection = () => run(() => toggleClipsMuted(commandsApi, selectedClips));

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
      selection = new Set([result.clipId]);
    });

  /** Plugin instruments, which the instrument picker offers next to the built-in synth. */
  const pluginInstruments = $derived(instrumentDescriptors(pluginDescriptors(descriptors)));

  const addInstrumentTrack = (instrument?: DeviceDescriptor) =>
    run(async () => {
      selectedTrack = await addMidiTrack(commandsApi, tracks.length, descriptors, instrument);
      selection = NO_SELECTION;
    });

  const newMidiClip = () =>
    run(async () => {
      const track = selectedMidiTrack;
      if (!track) return;
      const count = clipsByTrack.get(track.id)?.length ?? 0;
      selection = new Set([
        await createMidiClip(commandsApi, signatures, track, playheadBeat, count),
      ]);
    });

  const editNotes = (clip: Clip, notes: Note[]) =>
    run(() => commandsApi.edit(replaceNotesCommand(clip, notes)));

  const setParam = (device: { id: number }, param: ParamDescriptorDto, value: number) =>
    run(() => commandsApi.edit(setParamCommand(device, param, value)));

  const addEffect = (ref: MixerRef, descriptor: DeviceDescriptor) =>
    run(async () => {
      const strip = project ? stripOf(project, ref) : undefined;
      if (strip) selectedDevice = await addStripInsert(commandsApi, ref, strip, descriptor);
    });

  /** Changes one strip settings (a bus or the master; tracks have `setTrackStrip`). */
  const setStrip = (ref: MixerRef, change: Partial<StripSettings>) =>
    run(async () => {
      const strip = project ? stripOf(project, ref) : undefined;
      if (strip) await commandsApi.edit(setStripCommand(ref, strip.settings, change));
    });

  const addBusStrip = () =>
    run(async () => {
      const id = await addBus(commandsApi, buses.length);
      selectBus(id);
    });

  const setSends = (ref: MixerRef, change: (sends: Strip["sends"]) => Strip["sends"]) =>
    run(async () => {
      const strip = project ? stripOf(project, ref) : undefined;
      if (strip) await commandsApi.edit(setSendsCommand(ref, change(strip.sends)));
    });

  const deleteBus = (bus: Bus) =>
    run(async () => {
      if (!project) return;
      await removeBus(commandsApi, project, bus.id);
      if (selectedBus === bus.id) selectedBus = null;
    });

  const removeEffect = (device: Device) => run(() => commandsApi.edit(removeInsertCommand(device)));

  const bypassEffect = (device: Device) => run(() => commandsApi.edit(toggleBypassCommand(device)));

  function openRoll(clip: Clip) {
    if (clip.content.type !== "midi") return;
    if (selectedTrack !== clip.track) selectedDevice = null;
    selectedTrack = clip.track;
    selection = new Set([clip.id]);
    openClip = clip.id;
  }

  /** Dev benchmark: sweeps the timeline sideways and the track list up and down. */
  function benchmarkStep(elapsedMs: number) {
    const range = Math.max(
      0,
      projectEndBeat(project?.clips.values() ?? []) - width / view.pxPerBeat,
    );
    view = { ...view, originBeat: pingPong(elapsedMs, range, 4000) };
    if (scroller) {
      scroller.scrollTop = pingPong(elapsedMs, scroller.scrollHeight - scroller.clientHeight, 5000);
    }
  }

  // The scroller is a new element whenever the project opens: read its position, not the old one.
  $effect(() => {
    if (scroller) scrollTop = scroller.scrollTop;
  });

  onMount(() => {
    const reload = () =>
      loadDescriptors()
        .then((loaded) => (descriptors = loaded))
        .catch(() => {});
    reload();
    const removePerfHook = installPerfHook(benchmarkStep);
    // Audio files dropped from the OS: import them where they landed.
    let stopDrop = () => {};
    let gone = false;
    if (tauriTransport.available) {
      getCurrentWebview()
        .onDragDropEvent((event) => {
          if (event.payload.type === "drop") {
            onFilesDropped(event.payload.paths, event.payload.position);
          }
        })
        .then((unlisten) => (gone ? unlisten() : (stopDrop = unlisten)))
        .catch(() => {});
    }
    // A plugin scan finished (at startup, or from the settings or an agent): fetch again.
    let stop = () => {};
    watchPlugins(() => {
      resetDescriptors();
      reload();
    }).then((unlisten) => (gone ? unlisten() : (stop = unlisten)));
    return () => {
      gone = true;
      stop();
      stopDrop();
      removePerfHook();
    };
  });

  /** Imports dropped audio files at the track and beat under the drop (physical pixels). */
  function onFilesDropped(paths: string[], position: { x: number; y: number }) {
    if (!project || !scroller || rollClip) return;
    const ratio = window.devicePixelRatio || 1;
    const rect = scroller.getBoundingClientRect();
    const target = dropTarget(
      position.x / ratio,
      position.y / ratio,
      {
        left: rect.left,
        top: rect.top,
        width: rect.width,
        height: rect.height,
        scrollTop,
        headerPx: HEADER_PX,
        topPx: TOP_PX,
        lanePx: LANE_PX,
      },
      view,
      tracks,
      signatures,
      snap,
    );
    if (target === null) return;
    if (target === "midi") {
      onError("Audio files can only be dropped on audio tracks.");
      return;
    }
    void run(async () => {
      const result = await importAudioFiles(commandsApi, tempo, paths, {
        trackId: target.trackId,
        trackCount: tracks.length,
        start: target.beat,
      });
      if (!result) return;
      selectedTrack = result.trackId;
      selection = new Set(result.clipIds);
    });
  }

  function selectTrack(id: number) {
    if (selectedTrack !== id || selectedBus !== null) selectedDevice = null;
    selectedTrack = id;
    selectedBus = null;
    selection = NO_SELECTION;
  }

  /** Selects a bus or the master: the dock shows its FX chain (and the sends of a bus). */
  function selectBus(id: number | "master") {
    if (selectedBus !== id) selectedDevice = null;
    selectedBus = id;
    selectedTrack = null;
    selection = NO_SELECTION;
    openClip = null;
  }
</script>

<!-- A drag whose lane vanished never sees its pointer-up; this cancels it (a normal drop has
     already ended the drag in the lane, which runs first). -->
<svelte:window
  onkeydown={onKey}
  onpointerup={() => dragging && onDragEnd(false)}
  onpointercancel={() => dragging && onDragEnd(false)}
/>

<main class="arrangement" aria-label="Arrangement">
  <div class="toolbar">
    <button type="button" onclick={addAudioTrack} disabled={!project}>Add audio track</button>
    <button type="button" onclick={() => addInstrumentTrack()} disabled={!project || descriptors.length === 0}>
      Add MIDI track
    </button>
    {#if pluginInstruments.length > 0}
      <select
        aria-label="Add MIDI track with a plugin instrument"
        value=""
        disabled={!project}
        onchange={(event) => {
          const kind = event.currentTarget.value;
          event.currentTarget.value = "";
          const instrument = pluginInstruments.find((d) => d.kind === kind);
          if (instrument) addInstrumentTrack(instrument);
        }}
      >
        <option value="" disabled>MIDI track with plugin&hellip;</option>
        {#each pluginInstruments as instrument (instrument.kind)}
          <option value={instrument.kind}>{deviceLabel(instrument)}</option>
        {/each}
      </select>
    {/if}
    <button type="button" onclick={newMidiClip} disabled={!selectedMidiTrack}>New MIDI clip</button>
    <button type="button" onclick={addBusStrip} disabled={!project}>Add bus</button>
    <button type="button" onclick={importFile} disabled={!project}>Import audio&hellip;</button>
    <button
      type="button"
      title="Split the selected clips at the playhead (S)"
      onclick={splitAtPlayhead}
      disabled={splitTargets.length === 0}
    >
      Split
    </button>
    <button
      type="button"
      title="Copy the selected clips to just after them (Ctrl/Cmd+D)"
      onclick={duplicateSelection}
      disabled={selectedClips.length === 0}
    >
      Duplicate
    </button>
    <button
      type="button"
      title="Mute or unmute the selected clips (0)"
      onclick={muteSelection}
      disabled={selectedClips.length === 0}
    >
      Mute clip
    </button>
    <label class="snap">
      Clip gain
      <input
        type="number"
        aria-label="Clip gain in dB"
        title="Gain of the selected audio clips in dB"
        min={MIN_GAIN_DB}
        max={MAX_GAIN_DB}
        step="0.5"
        value={sharedGain ?? ""}
        placeholder={selectedAudio.length > 0 ? "mixed" : ""}
        disabled={selectedAudio.length === 0}
        onchange={(event) => {
          const value = event.currentTarget.valueAsNumber;
          if (Number.isNaN(value)) return;
          void run(() => setClipsGain(commandsApi, selectedAudio, value));
        }}
      />
    </label>
    <span class="spacer"></span>
    <button
      type="button"
      title="Add a marker at the playhead (M; hold Alt to skip the snap grid)"
      onclick={(event) => addMarkerAtPlayhead(event.altKey)}
      disabled={!project}
    >
      Add marker
    </button>
    <button
      type="button"
      aria-pressed={rulerMode === "time"}
      title="Show the ruler as bars and beats or as minutes and seconds"
      onclick={() => (rulerMode = rulerMode === "bars" ? "time" : "bars")}
    >
      {rulerMode === "bars" ? "Ruler: bars" : "Ruler: time"}
    </button>
    <label class="snap">
      Snap
      <select
        aria-label="Snap grid"
        value={snap}
        onchange={(event) => {
          const value = event.currentTarget.value;
          if (isSnapResolution(value)) snap = value;
        }}
      >
        {#each SNAP_OPTIONS as option (option.id)}
          <option value={option.id}>{option.label}</option>
        {/each}
      </select>
    </label>
    <button type="button" aria-label="Zoom out" title="Zoom out" onclick={() => zoomBy(1 / ZOOM_STEP)}>&minus;</button>
    <button type="button" aria-label="Zoom in" title="Zoom in" onclick={() => zoomBy(ZOOM_STEP)}>+</button>
  </div>

  {#if !project}
    <p class="empty">No project loaded.</p>
  {:else}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="scroller"
      bind:this={scroller}
      bind:clientHeight={scrollHeight}
      onwheel={onWheel}
      onscroll={() => (scrollTop = scroller?.scrollTop ?? 0)}
    >
      <div class="content">
        <div class="ruler-block">
          <div class="row ruler-row" style:height="{RULER_PX}px">
            <div class="corner" style:width="{HEADER_PX}px"></div>
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
              class="ruler"
              bind:clientWidth={width}
              title="Click to place the playhead. Shift+drag sets the loop range; drag the loop bar to edit it, double-click it to clear."
              onpointerdown={onRulerDown}
              onpointermove={moveRange}
              onpointerup={endRange}
              onpointercancel={endRange}
              ondblclick={(event) => {
                const rect = event.currentTarget.getBoundingClientRect();
                if (event.clientY - rect.top >= RULER_PX - LOOP_BAR_PX && loopRange) clearRange("loop");
              }}
            >
              {#if loopRange}
                <div
                  class="range loop"
                  style:left="{beatToX(view, loopRange.start)}px"
                  style:width="{(loopRange.end - loopRange.start) * view.pxPerBeat}px"
                  style:height="{LOOP_BAR_PX}px"
                  aria-hidden="true"
                ></div>
              {/if}
              {#if rulerMode === "bars"}
                {#each marks as mark (mark.kind + mark.beat)}
                  <div class="mark {mark.kind}" style:left="{mark.x}px">
                    {#if mark.label}<span>{mark.label}</span>{/if}
                  </div>
                {/each}
              {:else}
                {#each clockMarks as mark (mark.seconds)}
                  <div class="mark {mark.major ? 'bar' : 'beat'}" style:left="{mark.x}px">
                    {#if mark.label}<span>{mark.label}</span>{/if}
                  </div>
                {/each}
              {/if}
            </div>
          </div>
          <div class="row punch-row" style:height="{PUNCH_PX}px">
            <div class="corner marker-corner" style:width="{HEADER_PX}px">Punch</div>
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
              class="punch-strip"
              title="Drag to set the punch range; drag its ends or body to edit it; double-click to clear it"
              onpointerdown={onPunchDown}
              onpointermove={moveRange}
              onpointerup={endRange}
              onpointercancel={endRange}
              ondblclick={() => clearRange("punch")}
            >
              {#if punchRange}
                <div
                  class="range punch"
                  class:off={!settings?.punch_enabled}
                  style:left="{beatToX(view, punchRange.start)}px"
                  style:width="{(punchRange.end - punchRange.start) * view.pxPerBeat}px"
                  aria-hidden="true"
                ></div>
              {/if}
            </div>
          </div>
          <div class="row marker-row" style:height="{MARKER_PX}px">
            <div class="corner marker-corner" style:width="{HEADER_PX}px">Markers</div>
            <div class="markers">
              {#each shownMarkers as marker (marker.id)}
                <div class="marker" style:left="{beatToX(view, marker.beat)}px" style:--marker={cssColor(marker.color)}>
                  {#if editingMarker === marker.id}
                    <input
                      class="marker-name"
                      aria-label="Marker name"
                      maxlength="256"
                      bind:value={markerDraft}
                      use:focusOnMount
                      onkeydown={(event) => markerKey(event, marker)}
                      onblur={() => finishMarkerRename(marker, true)}
                    />
                  {:else}
                    <button
                      type="button"
                      class="flag"
                      title="{marker.name} (click to go there, double-click to rename)"
                      onclick={() => onLocate(marker.beat)}
                      ondblclick={() => startMarkerRename(marker)}
                    >
                      {marker.name}
                    </button>
                    <button
                      type="button"
                      class="flag-delete"
                      title="Delete marker"
                      aria-label="Delete marker {marker.name}"
                      onclick={() => run(() => removeMarker(commandsApi, marker.id))}
                    >
                      &times;
                    </button>
                  {/if}
                </div>
              {/each}
            </div>
          </div>
        </div>

        {#if firstRow > 0}
          <div style:height="{firstRow * LANE_PX}px" aria-hidden="true"></div>
        {/if}
        {#each visibleTracks as track (track.id)}
          <div class="row">
            <div class="head" style:width="{HEADER_PX}px">
              <TrackHeader
                {track}
                height={LANE_PX}
                selected={trackSelected(track.id)}
                armed={armed.has(track.id)}
                armLocked={recording}
                onArm={() => run(() => toggleArm(recordApi, armed, track.id))}
                {inputChannels}
                onInput={(input) => run(() => commandsApi.edit(setTrackInputCommand(track, input)))}
                onMonitor={(mode) => run(() => commandsApi.edit(setTrackMonitorCommand(track, mode)))}
                onSelect={() => selectTrack(track.id)}
                onRename={(name) => run(() => renameTrack(commandsApi, track, name))}
                destinations={routeTargets(project, { track: track.id })}
                onMute={(mute) => run(() => setTrackStrip(commandsApi, track, { mute }))}
                onSolo={(solo) => run(() => setTrackStrip(commandsApi, track, { solo }))}
                onVolume={(gain_db) => run(() => setTrackStrip(commandsApi, track, { gain_db }))}
                onPan={(pan) => run(() => setTrackStrip(commandsApi, track, { pan }))}
                onOutput={(output) => run(() => setTrackStrip(commandsApi, track, { output }))}
                onDelete={() => run(() => removeTrack(commandsApi, track.id))}
                onColor={(color) => run(() => setTrackColor(commandsApi, track, color))}
                canMoveUp={reorderTarget(trackIds, track.id, -1) !== null}
                canMoveDown={reorderTarget(trackIds, track.id, 1) !== null}
                onMoveUp={() => moveBy(trackIds, track.id, -1, moveTrack)}
                onMoveDown={() => moveBy(trackIds, track.id, 1, moveTrack)}
              />
            </div>
            <div class="lane-cell">
              <Lane
                {track}
                clips={visibleClips(laneByTrack.get(track.id) ?? [], view, width, liveSelection)}
                media={project.media_pool}
                {missing}
                {view}
                {width}
                height={LANE_PX}
                {tempo}
                selection={liveSelection}
                {moving}
                {preview}
                onSelectTrack={() => selectTrack(track.id)}
                onPress={(clip, additive) => onPressClip(track, clip, additive)}
                onRelease={onReleaseClip}
                onDrag={onDragClip}
                {onDragEnd}
                onOpenClip={openRoll}
                badgeOf={(clip) => badgeFor(clip, takes)}
                onTake={(clip, index) => run(() => chooseTake(commandsApi, clip, index))}
              />
            </div>
          </div>
        {/each}
        {#if tracks.length > lastRow}
          <div style:height="{(tracks.length - lastRow) * LANE_PX}px" aria-hidden="true"></div>
        {/if}

        {#each buses as bus (bus.id)}
          <div class="row">
            <div class="head" style:width="{HEADER_PX}px">
              <BusHeader
                name={bus.name}
                color={bus.color}
                master={false}
                settings={bus.strip.settings}
                height={LANE_PX}
                selected={dockBus !== null && sameStrip(dockBus, { bus: bus.id })}
                destinations={routeTargets(project, { bus: bus.id })}
                onSelect={() => selectBus(bus.id)}
                onRename={(name) => run(() => renameBus(commandsApi, bus, name))}
                onMute={(mute) => setStrip({ bus: bus.id }, { mute })}
                onVolume={(gain_db) => setStrip({ bus: bus.id }, { gain_db })}
                onPan={(pan) => setStrip({ bus: bus.id }, { pan })}
                onOutput={(output) => setStrip({ bus: bus.id }, { output })}
                onDelete={() => deleteBus(bus)}
                onColor={(color) => run(() => setBusColor(commandsApi, bus, color))}
                canMoveUp={reorderTarget(busIds, bus.id, -1) !== null}
                canMoveDown={reorderTarget(busIds, bus.id, 1) !== null}
                onMoveUp={() => moveBy(busIds, bus.id, -1, moveBus)}
                onMoveDown={() => moveBy(busIds, bus.id, 1, moveBus)}
              />
            </div>
            <div class="lane-cell bus-lane" style:height="{LANE_PX}px"></div>
          </div>
        {/each}

        <div class="row">
          <div class="head" style:width="{HEADER_PX}px">
            <BusHeader
              name="Master"
              color={0}
              master={true}
              settings={project.master.settings}
              height={LANE_PX}
              selected={dockBus === "master"}
              destinations={[]}
              onSelect={() => selectBus("master")}
              onRename={() => {}}
              onMute={(mute) => setStrip("master", { mute })}
              onVolume={(gain_db) => setStrip("master", { gain_db })}
              onPan={(pan) => setStrip("master", { pan })}
              onOutput={() => {}}
              onDelete={() => {}}
            />
          </div>
          <div class="lane-cell bus-lane" style:height="{LANE_PX}px"></div>
        </div>

        {#if tracks.length === 0}
          <p class="empty">Add an audio or MIDI track, or import an audio file, to begin.</p>
        {/if}

        {#if playheadX >= 0 && playheadX <= width}
          <div class="playhead" style:left="{HEADER_PX + playheadX}px" aria-hidden="true"></div>
        {/if}
      </div>
    </div>
    {#if dockRef && dockStrip}
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
            {dockTrack
              ? dockTrack.kind === "midi"
                ? "Double-click a MIDI clip to edit its notes."
                : "Add effects to the track from the FX list."
              : dockRef === "master"
                ? "Master effects process the whole mix. Put a limiter last."
                : "Bus effects process everything routed or sent to the bus."}
          </p>
        {/if}
        <DeviceChain
          {chain}
          shownId={dockDevice?.id ?? null}
          {effects}
          onSelect={(device) => (selectedDevice = device.id)}
          onAdd={(descriptor) => addEffect(dockRef, descriptor)}
          onRemove={removeEffect}
          onBypass={bypassEffect}
        />
        {#if dockRef !== "master"}
          <SendsPanel
            sends={dockStrip.sends}
            {buses}
            targets={routeTargets(project, dockRef)}
            onAdd={(bus) => setSends(dockRef, (sends) => withSend(sends, bus))}
            onLevel={(index, level_db) =>
              setSends(dockRef, (sends) => changeSend(sends, index, { level_db }))}
            onPre={(index, pre_fader) =>
              setSends(dockRef, (sends) => changeSend(sends, index, { pre_fader }))}
            onRemove={(index) => setSends(dockRef, (sends) => withoutSend(sends, index))}
          />
        {/if}
        {#if dockDevice && dockDescriptor}
          <DevicePanel
            device={dockDevice}
            descriptor={dockDescriptor}
            onParam={(param, value) => setParam(dockDevice, param, value)}
            onEditor={() => run(() => pluginEditorOpen(dockDevice.id))}
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
  .ruler-block {
    position: sticky;
    top: 0;
    z-index: 2;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
  }
  .punch-row,
  .marker-row {
    border-top: 1px solid var(--border);
  }
  .marker-corner {
    box-sizing: border-box;
    padding: 2px 6px;
    font-size: 11px;
    color: var(--muted);
  }
  .markers {
    position: relative;
    flex: 1;
    min-width: 0;
    overflow: hidden;
  }
  .marker {
    position: absolute;
    top: 0;
    bottom: 0;
    display: flex;
    align-items: center;
    border-left: 2px solid var(--marker);
    white-space: nowrap;
  }
  .flag,
  .flag-delete {
    padding: 0 4px;
    font-size: 11px;
    line-height: 16px;
    border: none;
    background: transparent;
    color: var(--text);
  }
  .flag-delete {
    color: var(--muted);
  }
  .marker-name {
    width: 110px;
    font: inherit;
    font-size: 11px;
  }
  .snap {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .corner {
    flex: none;
    border-right: 1px solid var(--border);
  }
  .punch-strip {
    position: relative;
    flex: 1;
    min-width: 0;
    overflow: hidden;
    cursor: crosshair;
    touch-action: none;
  }
  .range {
    position: absolute;
    bottom: 0;
    pointer-events: none;
    box-sizing: border-box;
    border-left: 3px solid var(--accent);
    border-right: 3px solid var(--accent);
    background: color-mix(in srgb, var(--accent) 35%, transparent);
  }
  .range.punch {
    top: 0;
    border-color: var(--danger);
    background: color-mix(in srgb, var(--danger) 35%, transparent);
  }
  .range.punch.off {
    opacity: 0.4;
  }
  .ruler {
    touch-action: none;
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
  .bus-lane {
    box-sizing: border-box;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
  }
  .empty {
    margin: 24px;
    color: var(--muted);
  }
</style>
