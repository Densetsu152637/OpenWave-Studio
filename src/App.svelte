<script lang="ts">
  import { onMount } from "svelte";
  import MenuBar from "./lib/components/MenuBar.svelte";
  import Arrangement from "./lib/components/Arrangement.svelte";
  import Transport from "./lib/components/Transport.svelte";
  import StatusBar from "./lib/components/StatusBar.svelte";
  import SettingsDialog from "./routes/settings/SettingsDialog.svelte";
  import { nativeDialogs } from "./lib/arrange/dialogs";
  import { fileActions } from "./lib/arrange/files";
  import {
    mergeTransport,
    positionLabel,
    transportLocate,
    transportPlay,
    transportStop,
  } from "./lib/arrange/playback";
  import { commandsApi } from "./lib/arrange/edits";
  import { armedSet, canRecord, recordApi, startRecording } from "./lib/arrange/record";
  import {
    commitRange,
    defaultLoop,
    setSettings,
    stopNotice,
    withCountIn,
    withPunchEnabled,
  } from "./lib/record";
  import type { BeatRange } from "./lib/ipc/types";
  import { SignatureMap, TempoMap } from "./lib/arrange/time";
  import {
    engineStatus,
    exportMix,
    exportWav,
    offlineStatus,
    audioDevices,
    prefsGet,
    projectNew,
    projectOpen,
    projectSave,
    redo,
    setTestTone,
    undo,
    type EngineStatus,
  } from "./lib/ipc";
  import { applyTheme } from "./lib/prefs";
  import { inputChannelCount } from "./lib/record";
  import { modelMirror, telemetryStore, type MirrorState } from "./lib/stores";

  const POLL_MS = 500;
  /** When the UI last started or stopped the transport itself; older status replies are stale. */
  let actionAt = Number.NEGATIVE_INFINITY;
  const acted = () => (actionAt = performance.now());

  let status = $state<EngineStatus>(offlineStatus);
  let settingsOpen = $state(false);
  /** The Settings section to open at; the version button opens it at About › Updates. */
  let settingsTab = $state<"audio" | "about">("audio");
  let mirror = $state.raw<MirrorState>(modelMirror.state);
  let notice = $state<string | null>(null);
  let playing = $state(false);
  /** A recording is running: Stop ends it and the takes arrive as clips. */
  let recording = $state(false);
  /** The last transport position, in samples, from telemetry; null before the first frame. */
  let framePosition = $state<number | null>(null);
  /** Where the playhead was last placed by a click, used until telemetry reports a position. */
  let locatedBeat = $state(0);

  const tempo = $derived(new TempoMap(mirror.project?.tempo_map ?? []));
  const signatures = $derived(new SignatureMap(mirror.project?.time_signature_map ?? []));
  const sampleRate = $derived(status.sampleRate ?? mirror.project?.settings.sample_rate ?? 48_000);
  const playheadBeat = $derived(
    framePosition === null ? locatedBeat : tempo.samplesToBeats(framePosition, sampleRate),
  );
  const projectName = $derived(mirror.project?.name ?? "No project");
  // A new or opened project replaces the one that was playing: stop, so Play is usable again.
  const stopAfter = (op: () => Promise<unknown>) => async () => {
    const result = await op();
    acted();
    playing = false;
    recording = false;
    await transportStop().catch(() => {});
    return result;
  };
  const files = fileActions(
    {
      projectNew: stopAfter(projectNew) as typeof projectNew,
      projectOpen: (path) => stopAfter(() => projectOpen(path))() as ReturnType<typeof projectOpen>,
      projectSave,
      exportWav,
      exportMix: (path, normalize) => exportMix({ path, normalize }),
    },
    nativeDialogs,
    () => ({
      dirty: mirror.history?.dirty ?? false,
      projectPath: mirror.projectPath,
      name: mirror.project?.name ?? "Untitled",
    }),
  );

  const fail = (error: unknown) => (notice = String(error));

  async function play() {
    try {
      await transportPlay();
      acted();
      playing = true;
    } catch (error) {
      playing = false;
      fail(error);
    }
  }

  async function record() {
    try {
      await startRecording(recordApi, armedSet(mirror.history));
      acted();
      recording = true;
      playing = true;
    } catch (error) {
      recording = false;
      fail(error);
    }
  }

  /** The loop range as it was before looping was turned off, so turning it on restores it. */
  /** Channels of the input device, for the tracks' input selectors; null without one. */
  let inputChannels = $state<number | null>(null);

  async function refreshInputChannels() {
    try {
      const [snapshot, devices] = await Promise.all([prefsGet(), audioDevices()]);
      inputChannels = inputChannelCount(devices, snapshot.prefs.audio);
    } catch {
      inputChannels = null;
    }
  }

  let lastLoop: BeatRange | null = null;
  const settings = $derived(mirror.project?.settings);

  /** Runs a settings change as one undo step, reporting a failure. */
  async function editSettings(change: () => Promise<unknown>) {
    try {
      await change();
    } catch (error) {
      fail(error);
    }
  }

  const setCountIn = (bars: number) =>
    settings &&
    editSettings(() => setSettings(commandsApi, "Set count-in", withCountIn(settings, bars)));

  const togglePunch = () =>
    settings &&
    editSettings(() =>
      setSettings(
        commandsApi,
        settings.punch_enabled ? "Punch off" : "Punch on",
        withPunchEnabled(settings, !settings.punch_enabled),
      ),
    );

  function toggleLoop() {
    if (!settings) return;
    const current = settings;
    if (current.loop_range) lastLoop = current.loop_range;
    const next = current.loop_range ? null : (lastLoop ?? defaultLoop(playheadBeat, signatures));
    void editSettings(() => commitRange(commandsApi, current, "loop", next));
  }

  async function stop() {
    const wasRecording = recording;
    try {
      // While recording this also finishes the takes and adds them to the project.
      await transportStop();
    } catch (error) {
      // After a recording this lists what went wrong; the takes that could be added were.
      notice = stopNotice(error, wasRecording);
    } finally {
      // Whatever the engine is doing, Play must stay usable.
      acted();
      playing = false;
      recording = false;
    }
  }

  async function locate(beat: number) {
    locatedBeat = beat;
    try {
      await transportLocate(beat);
    } catch (error) {
      playing = false;
      fail(error);
    }
  }

  function onKey(event: KeyboardEvent) {
    const target = event.target as HTMLElement | null;
    const typing = target?.closest("input, textarea, select, [contenteditable]");
    const command = event.ctrlKey || event.metaKey;
    const key = event.key.toLowerCase();
    if (command && !typing && (key === "z" || key === "y")) {
      event.preventDefault();
      const redoing = key === "y" || event.shiftKey;
      void (redoing ? redo() : undo()).catch(fail);
    } else if (event.key === " " && !command && !typing && !target?.closest("button")) {
      event.preventDefault();
      void (playing ? stop() : play());
    }
  }

  async function refresh() {
    try {
      const issuedAt = performance.now();
      const reply = await engineStatus();
      status = reply;
      // The engine's own state wins: it stops by itself, and agents play and stop it too.
      const next = mergeTransport({ playing, recording }, reply, issuedAt, actionAt);
      playing = next.playing;
      recording = next.recording;
    } catch (error) {
      status = { ...offlineStatus, error: String(error) };
      playing = false;
      recording = false;
    }
  }

  async function toggleTone(on: boolean) {
    try {
      status = await setTestTone(on);
    } catch (error) {
      status = { ...status, error: String(error) };
    }
  }

  onMount(() => {
    void refresh();
    void refreshInputChannels();
    // The project mirror: patches (including engine swap failures) arrive on its channel.
    const stopMirror = modelMirror.subscribe((state) => (mirror = state));
    void modelMirror.attach();
    // The transport position for the playhead.
    const stopTelemetry = telemetryStore.subscribe((frame) => (framePosition = frame?.position ?? null));
    telemetryStore.start().catch(() => {});
    // The saved theme applies at launch, not only when Settings opens.
    prefsGet()
      .then((snapshot) => applyTheme(snapshot.prefs.appearance.theme))
      .catch(() => {});
    const timer = setInterval(refresh, POLL_MS);
    return () => {
      clearInterval(timer);
      stopMirror();
      stopTelemetry();
    };
  });
</script>

<svelte:window onkeydown={onKey} />

<div class="shell">
  <MenuBar
    onSettings={() => {
      settingsTab = "audio";
      settingsOpen = true;
    }}
    onUpdates={() => {
      settingsTab = "about";
      settingsOpen = true;
    }}
    file={files}
    title={mirror.history?.dirty ? `${projectName} *` : projectName}
    hasProject={mirror.project !== null}
    onError={fail}
  />
  <Transport
    {status}
    onToggle={toggleTone}
    {playing}
    position={positionLabel(playheadBeat, signatures)}
    {recording}
    canRecord={canRecord(armedSet(mirror.history), recording)}
    onPlay={play}
    onRecord={record}
    onStop={stop}
    countIn={settings?.count_in_bars ?? 0}
    punchOn={settings?.punch_enabled ?? false}
    punchSet={settings?.punch_range != null}
    loopOn={settings?.loop_range != null}
    onCountIn={setCountIn}
    onPunch={togglePunch}
    onLoop={toggleLoop}
  />
  <Arrangement {mirror} {playheadBeat} {recording} {inputChannels} onLocate={locate} onError={fail} />
  <StatusBar {status} engineError={mirror.history?.engineError ?? null} />
</div>

{#if notice}
  <div class="notice" role="alert">
    <span>{notice}</span>
    <button type="button" onclick={() => (notice = null)}>Dismiss</button>
  </div>
{/if}

{#if settingsOpen}
  <SettingsDialog initialTab={settingsTab} {status} onStatus={(next) => (status = next)} onClose={() => {
      settingsOpen = false;
      void refreshInputChannels();
    }} />
{/if}

<style>
  .notice {
    position: fixed;
    right: 12px;
    bottom: 36px;
    z-index: 20;
    display: flex;
    align-items: center;
    gap: 10px;
    max-width: 60%;
    padding: 6px 10px;
    background: var(--panel-2);
    border: 1px solid var(--danger);
    border-radius: 4px;
    color: var(--danger);
    user-select: text;
  }
  .shell {
    display: grid;
    grid-template-rows: auto auto 1fr auto;
    height: 100%;
  }
</style>
