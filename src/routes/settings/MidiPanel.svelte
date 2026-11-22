<script lang="ts">
  import type { DeviceLists, EngineStatus, Preferences } from "../../lib/ipc";
  import { setMidiInput } from "../../lib/prefs";

  let {
    prefs = $bindable(),
    lists,
    status,
  }: { prefs: Preferences; lists: DeviceLists; status: EngineStatus } = $props();

  const stored = (port: string) => prefs.midi.inputs.find((input) => input.port === port);
  // Ports saved in the preferences but not plugged in right now.
  const missing = $derived(
    prefs.midi.inputs.filter((input) => !lists.midiInputs.some((port) => port.id === input.port)),
  );
  const portStatus = (port: string) => status.midi.ports.find((entry) => entry.port === port);
  const hex = (value: number) => value.toString(16).toUpperCase().padStart(2, "0");
</script>

<section aria-labelledby="midi-heading">
  <h3 id="midi-heading">MIDI inputs</h3>

  {#if lists.midiError}
    <p class="warn">MIDI is unavailable: {lists.midiError}</p>
  {:else if lists.midiInputs.length === 0 && missing.length === 0}
    <p class="hint">No MIDI input devices found. Plug one in and reopen Settings.</p>
  {/if}

  <ul>
    {#each lists.midiInputs as port (port.id)}
      {@const entry = stored(port.id)}
      {@const state = portStatus(port.id)}
      <li>
        <label>
          <input
            type="checkbox"
            checked={entry?.enabled ?? false}
            onchange={(event) =>
              (prefs.midi.inputs = setMidiInput(prefs.midi.inputs, port.id, port.name, {
                enabled: event.currentTarget.checked,
              }))}
          />
          {port.name}
        </label>
        <label class="small">
          <input
            type="checkbox"
            checked={entry?.controlSurface ?? false}
            disabled={!entry?.enabled}
            onchange={(event) =>
              (prefs.midi.inputs = setMidiInput(prefs.midi.inputs, port.id, port.name, {
                controlSurface: event.currentTarget.checked,
              }))}
          />
          Control surface
        </label>
        {#if entry?.enabled && state}
          <span class="badge" class:warn={!state.connected}>
            {state.connected ? "connected" : (state.error ?? "not connected")}
          </span>
        {/if}
      </li>
    {/each}
    {#each missing as input (input.port)}
      <li class="absent">
        <label>
          <input
            type="checkbox"
            checked={input.enabled}
            onchange={(event) =>
              (prefs.midi.inputs = setMidiInput(prefs.midi.inputs, input.port, input.name, {
                enabled: event.currentTarget.checked,
              }))}
          />
          {input.name || input.port} (not plugged in)
        </label>
      </li>
    {/each}
  </ul>

  <p class="hint" data-testid="midi-monitor">
    Received {status.midi.events} messages{#if status.midi.dropped > 0}, {status.midi.dropped} dropped{/if}{#if status.midi.last}.
      Last: {hex(status.midi.last[0])} {hex(status.midi.last[1])} {hex(status.midi.last[2])}{/if}
  </p>
  <p class="hint">
    Every input gets its own path into the audio engine. Control surfaces share one dedicated path.
    Changes apply when you press Apply.
  </p>
</section>

<style>
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }
  li {
    display: flex;
    align-items: center;
    gap: 14px;
    flex-wrap: wrap;
  }
  .small {
    color: var(--muted);
  }
  .absent {
    color: var(--muted);
  }
  .badge {
    color: var(--muted);
  }
  .hint {
    color: var(--muted);
    margin: 10px 0 0;
  }
  .warn {
    color: var(--danger);
  }
</style>
