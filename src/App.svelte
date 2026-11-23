<script lang="ts">
  import { onMount } from "svelte";
  import MenuBar from "./lib/components/MenuBar.svelte";
  import Arrangement from "./lib/components/Arrangement.svelte";
  import Transport from "./lib/components/Transport.svelte";
  import StatusBar from "./lib/components/StatusBar.svelte";
  import SettingsDialog from "./routes/settings/SettingsDialog.svelte";
  import { nativeDialogs } from "./lib/arrange/dialogs";
  import { fileActions } from "./lib/arrange/files";
  import { positionLabel, transportLocate, transportPlay, transportStop } from "./lib/arrange/playback";
  import { armedSet, canRecord, recordApi, startRecording } from "./lib/arrange/record";
  import { SignatureMap, TempoMap } from "./lib/arrange/time";
  import {
    engineStatus,
    exportMix,
    exportWav,
    offlineStatus,
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
  import { modelMirror, telemetryStore, type MirrorState } from "./lib/stores";

  const POLL_MS = 500;

  let status = $state<EngineStatus>(offlineStatus);
  let settingsOpen = $state(false);
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
      playing = true;
    } catch (error) {
      playing = false;
      fail(error);
    }
  }

  async function record() {
    try {
      await startRecording(recordApi, armedSet(mirror.history));
      recording = true;
      playing = true;
    } catch (error) {
      recording = false;
      fail(error);
    }
  }

  async function stop() {
    try {
      // While recording this also finishes the takes and adds them to the project.
      await transportStop();
    } catch (error) {
      fail(error);
    } finally {
      // Whatever the engine is doing, Play must stay usable.
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
      status = await engineStatus();
    } catch (error) {
      status = { ...offlineStatus, error: String(error) };
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
    onSettings={() => (settingsOpen = true)}
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
  />
  <Arrangement {mirror} {playheadBeat} {recording} onLocate={locate} onError={fail} />
  <StatusBar {status} engineError={mirror.history?.engineError ?? null} />
</div>

{#if notice}
  <div class="notice" role="alert">
    <span>{notice}</span>
    <button type="button" onclick={() => (notice = null)}>Dismiss</button>
  </div>
{/if}

{#if settingsOpen}
  <SettingsDialog {status} onStatus={(next) => (status = next)} onClose={() => (settingsOpen = false)} />
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
