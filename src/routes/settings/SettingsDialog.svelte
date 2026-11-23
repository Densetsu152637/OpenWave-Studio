<script lang="ts">
  import { onMount } from "svelte";
  import AiPanel from "./AiPanel.svelte";
  import AppearancePanel from "./AppearancePanel.svelte";
  import AudioPanel from "./AudioPanel.svelte";
  import MidiPanel from "./MidiPanel.svelte";
  import PathsPanel from "./PathsPanel.svelte";
  import ShortcutsPanel from "./ShortcutsPanel.svelte";
  import UpdatePanel from "./update/UpdatePanel.svelte";
  import {
    audioDevices,
    audioRetry,
    defaultPreferences,
    noDevices,
    offlineStatus,
    prefsGet,
    prefsSet,
    type DeviceLists,
    type EngineStatus,
    type Preferences,
  } from "../../lib/ipc";
  import { applyTheme, samePrefs } from "../../lib/prefs";

  type Tab = "audio" | "midi" | "folders" | "appearance" | "ai" | "shortcuts" | "about";

  let {
    onClose,
    status = offlineStatus,
    onStatus = () => {},
  }: {
    onClose: () => void;
    /** Live engine status, polled by the app. */
    status?: EngineStatus;
    onStatus?: (status: EngineStatus) => void;
  } = $props();

  const tabs: { id: Tab; label: string }[] = [
    { id: "audio", label: "Audio" },
    { id: "midi", label: "MIDI" },
    { id: "folders", label: "Folders" },
    { id: "appearance", label: "Appearance" },
    { id: "ai", label: "AI control" },
    { id: "shortcuts", label: "Shortcuts" },
    { id: "about", label: "About" },
  ];

  let tab = $state<Tab>("audio");
  let saved = $state<Preferences>(structuredClone(defaultPreferences));
  let draft = $state<Preferences>(structuredClone(defaultPreferences));
  let lists = $state<DeviceLists>(noDevices);
  let loading = $state(true);
  let applying = $state(false);
  let warning = $state<string | null>(null);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let configFile = $state("");

  const dirty = $derived(!samePrefs(saved, draft));

  onMount(() => {
    void (async () => {
      try {
        const snapshot = await prefsGet();
        saved = snapshot.prefs;
        draft = structuredClone(snapshot.prefs);
        warning = snapshot.warning;
        configFile = snapshot.configFile;
      } catch (cause) {
        error = String(cause);
      }
      try {
        lists = await audioDevices();
      } catch (cause) {
        error = `Could not list devices: ${String(cause)}`;
      } finally {
        loading = false;
      }
    })();
  });

  async function apply() {
    applying = true;
    error = null;
    notice = null;
    try {
      const result = await prefsSet(draft);
      saved = result.prefs;
      draft = structuredClone(result.prefs);
      warning = null;
      onStatus(result.status);
      if (result.audioError) {
        error = `The audio device could not be changed (${result.audioError}). The previous device is still in use.`;
      } else {
        notice = "Saved.";
      }
    } catch (cause) {
      error = String(cause);
    } finally {
      applying = false;
    }
  }

  function revert() {
    draft = structuredClone(saved);
    applyTheme(saved.appearance.theme);
    error = null;
    notice = null;
  }

  function close() {
    // An unapplied theme preview must not outlive the dialog.
    applyTheme(saved.appearance.theme);
    onClose();
  }

  async function retry() {
    try {
      onStatus(await audioRetry());
    } catch (cause) {
      error = String(cause);
    }
  }
</script>

<svelte:window onkeydown={(event) => event.key === "Escape" && close()} />

<div class="backdrop" role="presentation" onclick={close}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-label="Settings"
    tabindex="-1"
    onclick={(event) => event.stopPropagation()}
    onkeydown={(event) => event.stopPropagation()}
  >
    <header>
      <h2>Settings</h2>
      <button type="button" onclick={close} aria-label="Close settings">Close</button>
    </header>

    <div class="tabs" role="tablist" aria-label="Settings sections">
      {#each tabs as entry (entry.id)}
        <button
          type="button"
          role="tab"
          id={`tab-${entry.id}`}
          aria-selected={tab === entry.id}
          aria-controls={`panel-${entry.id}`}
          class:active={tab === entry.id}
          onclick={() => (tab = entry.id)}
        >
          {entry.label}
        </button>
      {/each}
    </div>

    {#if warning}
      <p class="banner bad" role="alert">{warning}</p>
    {/if}

    <div class="panel" role="tabpanel" id={`panel-${tab}`} aria-labelledby={`tab-${tab}`}>
      {#if tab === "about"}
        <UpdatePanel />
      {:else if loading && tab !== "appearance" && tab !== "ai" && tab !== "shortcuts" && tab !== "folders"}
        <p class="hint">Loading devices...</p>
      {:else if tab === "audio"}
        <AudioPanel bind:prefs={draft} {lists} {status} onRetry={retry} />
      {:else if tab === "midi"}
        <MidiPanel bind:prefs={draft} {lists} {status} />
      {:else if tab === "folders"}
        <PathsPanel bind:prefs={draft} />
      {:else if tab === "appearance"}
        <AppearancePanel bind:prefs={draft} />
      {:else if tab === "ai"}
        <AiPanel bind:prefs={draft} />
      {:else}
        <ShortcutsPanel bind:prefs={draft} />
      {/if}
    </div>

    {#if tab !== "about"}
      <footer>
        <span class="message" class:bad={error !== null} role="status">{error ?? notice ?? ""}</span>
        <button type="button" onclick={revert} disabled={!dirty || applying}>Revert</button>
        <button type="button" class="primary" onclick={apply} disabled={!dirty || applying}>
          {applying ? "Applying..." : "Apply"}
        </button>
      </footer>
      {#if configFile}
        <p class="hint file">Stored in {configFile}</p>
      {/if}
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: rgba(0, 0, 0, 0.45);
    z-index: 10;
  }
  .dialog {
    width: min(680px, 94vw);
    max-height: 86vh;
    overflow: auto;
    padding: 12px 18px 14px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    user-select: text;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  h2 {
    margin: 0;
    font-size: 1.2em;
  }
  .tabs {
    display: flex;
    gap: 2px;
    margin: 10px 0;
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }
  .tabs button {
    border-radius: 4px 4px 0 0;
    border-bottom: none;
    background: transparent;
  }
  .tabs button.active {
    background: var(--panel-2);
    border-color: var(--accent);
  }
  .panel {
    min-height: 220px;
  }
  footer {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 14px;
  }
  .message {
    flex: 1;
    color: var(--muted);
  }
  .bad {
    color: var(--danger);
  }
  .banner {
    padding: 6px 8px;
    border: 1px solid var(--danger);
    border-radius: 4px;
    margin: 0 0 10px;
  }
  .primary {
    border-color: var(--accent);
  }
  .hint {
    color: var(--muted);
  }
  .file {
    margin: 8px 0 0;
    font-size: 0.9em;
    overflow-wrap: anywhere;
  }
</style>
