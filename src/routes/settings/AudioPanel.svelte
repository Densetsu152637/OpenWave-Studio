<script lang="ts">
  import type { DeviceLists, EngineStatus, Preferences } from "../../lib/ipc";
  import {
    activeBackend,
    bufferChoices,
    bufferMs,
    inputDevices,
    outputDevices,
    selectedOutput,
  } from "../../lib/prefs";

  let {
    prefs = $bindable(),
    lists,
    status,
    onRetry,
  }: {
    prefs: Preferences;
    lists: DeviceLists;
    status: EngineStatus;
    onRetry: () => void;
  } = $props();

  const backend = $derived(activeBackend(prefs, lists));
  const devices = $derived(backend?.devices ?? []);
  const outputs = $derived(outputDevices(devices));
  const inputs = $derived(inputDevices(devices));
  const output = $derived(selectedOutput(prefs, devices));
  const exclusive = $derived(backend?.id === "wasapi-exclusive");
  const rates = $derived(output?.sampleRates ?? []);
  const buffers = $derived(bufferChoices(output));
  const activeRate = $derived(prefs.audio.sampleRate ?? status.sampleRate);

  const toNumber = (value: string): number | null => (value === "" ? null : Number(value));
  const toDevice = (value: string): string | null => (value === "" ? null : value);

  function pickBackend(event: Event) {
    prefs.audio.backend = (event.currentTarget as HTMLSelectElement).value;
    // Device ids belong to one backend, so a new backend starts from its defaults.
    prefs.audio.outputDevice = null;
    prefs.audio.inputDevice = null;
    prefs.audio.sampleRate = null;
  }
</script>

<section aria-labelledby="audio-heading">
  <h3 id="audio-heading">Audio device</h3>

  <p class="state" class:bad={status.audioState !== "running"} role="status" data-testid="audio-state">
    {#if status.audioState === "running"}
      Running: {status.deviceName}
      {#if status.sampleRate}at {+(status.sampleRate / 1000).toFixed(1)} kHz{/if}
      {#if status.bufferFrames}, buffer {status.bufferFrames} frames ({bufferMs(status.bufferFrames, status.sampleRate)}){/if}
      {#if status.exclusive}, exclusive mode{/if}
      {#if status.onFallbackDevice}
        <span class="warn">The chosen device is unavailable; using the system default.</span>
      {/if}
    {:else if status.audioState === "recovering"}
      {status.error}
    {:else}
      Audio stopped: {status.error ?? "no device"}
      <button type="button" onclick={onRetry}>Retry</button>
    {/if}
  </p>

  <div class="grid">
    <label for="audio-host">Audio system</label>
    <select id="audio-host" value={backend?.id ?? ""} onchange={pickBackend} disabled={lists.backends.length === 0}>
      {#each lists.backends as host (host.id)}
        <option value={host.id}>{host.label}</option>
      {/each}
    </select>

    <label for="audio-output">Output device</label>
    <select
      id="audio-output"
      value={prefs.audio.outputDevice ?? ""}
      onchange={(event) => {
        prefs.audio.outputDevice = toDevice(event.currentTarget.value);
        prefs.audio.sampleRate = null;
      }}
    >
      <option value="">System default</option>
      {#each outputs as device (device.id)}
        <option value={device.id}>{device.name}{device.isDefaultOutput ? " (default)" : ""}</option>
      {/each}
    </select>

    <label for="audio-input">Input device</label>
    <select
      id="audio-input"
      value={prefs.audio.inputDevice ?? ""}
      disabled={exclusive}
      onchange={(event) => (prefs.audio.inputDevice = toDevice(event.currentTarget.value))}
    >
      <option value="">None</option>
      <option value={lists.defaultDevice}>System default</option>
      {#each inputs as device (device.id)}
        <option value={device.id}>{device.name}{device.isDefaultInput ? " (default)" : ""}</option>
      {/each}
    </select>

    <label for="audio-rate">Sample rate</label>
    <select
      id="audio-rate"
      value={prefs.audio.sampleRate === null ? "" : String(prefs.audio.sampleRate)}
      onchange={(event) => (prefs.audio.sampleRate = toNumber(event.currentTarget.value))}
    >
      <option value="">Device default</option>
      {#each rates as rate (rate)}
        <option value={String(rate)}>{+(rate / 1000).toFixed(1)} kHz</option>
      {/each}
    </select>

    <label for="audio-buffer">Buffer size</label>
    <select
      id="audio-buffer"
      value={prefs.audio.bufferFrames === null ? "" : String(prefs.audio.bufferFrames)}
      onchange={(event) => (prefs.audio.bufferFrames = toNumber(event.currentTarget.value))}
    >
      <option value="">Device default</option>
      {#each buffers as frames (frames)}
        <option value={String(frames)}>{frames} frames ({bufferMs(frames, activeRate)})</option>
      {/each}
    </select>
  </div>

  {#if exclusive}
    <p class="hint">
      Exclusive mode talks to the device directly for the lowest latency; no other application can
      use it while OpenWave Studio is running. Recording is not available in this mode yet.
    </p>
  {/if}
  {#if backend?.id === "cpal:wasapi"}
    <p class="hint">
      Shared WASAPI uses the buffer size Windows chooses for the device. Pick the exclusive mode
      for a smaller or custom buffer.
    </p>
  {/if}
  {#each lists.backends.filter((host) => host.error) as host (host.id)}
    <p class="hint warn">{host.label}: {host.error}</p>
  {/each}
  <p class="hint">Changes apply when you press Apply; audio pauses for a moment while the device reopens.</p>
</section>

<style>
  .grid {
    display: grid;
    grid-template-columns: 9em 1fr;
    gap: 8px 12px;
    align-items: center;
  }
  select {
    font: inherit;
    color: inherit;
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 3px 6px;
    min-width: 0;
  }
  .state {
    margin: 0 0 12px;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--panel-2);
  }
  .state.bad {
    border-color: var(--danger);
  }
  .hint {
    color: var(--muted);
    margin: 10px 0 0;
  }
  .warn {
    color: var(--danger);
  }
</style>
