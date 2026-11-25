<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { commandsApi } from "../arrange/edits";
  import type { TempoMap } from "../arrange/time";
  import {
    libraryPreview,
    libraryPreviewStop,
    libraryScan,
    libraryScanCancel,
    libraryScanStatus,
    librarySearch,
    prefsGet,
  } from "../ipc";
  import type { LibraryEntryInfo, LibraryScanStatus } from "../ipc/types";
  import { addLibraryEntry } from "./add";
  import {
    KEY_CHOICES,
    NO_FILTERS,
    bpmLabel,
    durationLabel,
    emptyHint,
    entryTitle,
    keyLabel,
    scanFraction,
    scanSummary,
    toQuery,
    type BrowserFilters,
  } from "./library";

  let {
    tempo,
    playheadBeat,
    selectedTrack,
    trackCount,
    onAdded,
    onError,
    onClose,
  }: {
    tempo: TempoMap;
    playheadBeat: number;
    /** The audio track a loop is added to; null adds a new audio track. */
    selectedTrack: number | null;
    trackCount: number;
    /** A loop was added: its clip and track. */
    onAdded: (result: { clipId: number; trackId: number; warped: boolean }) => void;
    onError: (message: string) => void;
    onClose: () => void;
  } = $props();

  /** Pause between asking for the scan progress. */
  const POLL_MS = 250;
  /** Pause after the last keystroke before the search runs. */
  const TYPING_MS = 150;

  let filters = $state<BrowserFilters>({ ...NO_FILTERS });
  let entries = $state.raw<LibraryEntryInfo[]>([]);
  let total = $state(0);
  let matched = $state(0);
  let folders = $state(0);
  let status = $state<LibraryScanStatus>({
    scanning: false,
    done: 0,
    total: 0,
    entries: 0,
    analysed: 0,
    cached: 0,
    failed: 0,
    cancelled: false,
    roots: [],
  });
  let current = $state<string | null>(null);
  let playing = $state<string | null>(null);
  let adding = $state(false);
  let poll: ReturnType<typeof setInterval> | undefined;
  let typing: ReturnType<typeof setTimeout> | undefined;
  let searchSeq = 0;

  const fraction = $derived(scanFraction(status));
  const selected = $derived(entries.find((entry) => entry.path === current) ?? null);

  async function search() {
    const seq = ++searchSeq;
    try {
      const reply = await librarySearch(toQuery(filters));
      if (seq !== searchSeq) return;
      entries = reply.entries;
      total = reply.total;
      matched = reply.matched;
    } catch (error) {
      onError(String(error));
    }
  }

  function typed() {
    clearTimeout(typing);
    typing = setTimeout(() => void search(), TYPING_MS);
  }

  async function scan(force: boolean) {
    if (status.scanning) return;
    status = { ...status, scanning: true, done: 0, total: 0 };
    poll = setInterval(async () => {
      try {
        const next = await libraryScanStatus();
        if (next.scanning) status = next;
      } catch {
        // The final answer below reports a real failure.
      }
    }, POLL_MS);
    try {
      status = await libraryScan(force);
    } catch (error) {
      status = { ...status, scanning: false };
      onError(String(error));
    } finally {
      clearInterval(poll);
      poll = undefined;
    }
    await search();
  }

  const cancel = () => void libraryScanCancel().catch((error) => onError(String(error)));

  async function stopPreview() {
    playing = null;
    try {
      await libraryPreviewStop();
    } catch (error) {
      onError(String(error));
    }
  }

  /** Click: select the file and play it in the project tempo; click again to stop it. */
  async function audition(entry: LibraryEntryInfo) {
    current = entry.path;
    if (playing === entry.path) return stopPreview();
    if (entry.error) return;
    try {
      await libraryPreview(entry.path);
      playing = entry.path;
    } catch (error) {
      playing = null;
      onError(String(error));
    }
  }

  async function add(entry: LibraryEntryInfo | null) {
    if (!entry || entry.error || adding) return;
    adding = true;
    try {
      const result = await addLibraryEntry(commandsApi, tempo, entry, {
        trackId: selectedTrack,
        trackCount,
        start: playheadBeat,
      });
      onAdded(result);
    } catch (error) {
      onError(String(error));
    } finally {
      adding = false;
    }
  }

  onMount(() => {
    prefsGet()
      .then((snapshot) => (folders = snapshot.prefs.paths.libraryDirs.length))
      .catch(() => {});
    libraryScanStatus()
      .then((next) => (status = next))
      .catch(() => {});
    void search();
  });

  onDestroy(() => {
    clearInterval(poll);
    clearTimeout(typing);
    if (playing !== null) void libraryPreviewStop().catch(() => {});
  });
</script>

<section class="browser" aria-label="Loop browser">
  <header>
    <strong>Loops</strong>
    <input
      type="search"
      class="text"
      aria-label="Search the library"
      placeholder="Search names and tags"
      bind:value={filters.text}
      oninput={typed}
    />
    <label>
      BPM
      <input
        type="number"
        class="num"
        aria-label="Lowest tempo"
        placeholder="min"
        min="1"
        bind:value={filters.bpmMin}
        oninput={typed}
      />
      <input
        type="number"
        class="num"
        aria-label="Highest tempo"
        placeholder="max"
        min="1"
        bind:value={filters.bpmMax}
        oninput={typed}
      />
    </label>
    <label title="Also list files at half or double a tempo in the range">
      <input type="checkbox" bind:checked={filters.halfDouble} onchange={typed} />
      &frac12;&times;/2&times;
    </label>
    <select
      aria-label="Key"
      bind:value={filters.key}
      onchange={() => void search()}
    >
      {#each KEY_CHOICES as choice (choice.value)}
        <option value={choice.value}>{choice.label}</option>
      {/each}
    </select>
    <span class="spacer"></span>
    {#if status.scanning}
      <button type="button" onclick={cancel}>Cancel scan</button>
    {:else}
      <button type="button" onclick={() => scan(false)} title="Read the library folders (unchanged files are quick)">
        Scan
      </button>
      <button type="button" onclick={() => scan(true)} title="Analyse every file again">Rescan all</button>
    {/if}
    <button type="button" onclick={onClose} aria-label="Close the loop browser">Close</button>
  </header>

  <div class="status" aria-live="polite">
    {#if status.scanning}
      <progress max="1" value={fraction ?? undefined} aria-label="Scan progress"></progress>
    {/if}
    <span>{scanSummary(status)}</span>
    {#if matched !== total && total > 0}<span>&middot; {matched} match</span>{/if}
  </div>

  <div class="list" role="table" aria-label="Library files">
    {#if entries.length === 0}
      <p class="empty">{emptyHint(total, status.scanning, folders)}</p>
    {:else}
      <div class="row head" role="row">
        <span class="name" role="columnheader">Name</span>
        <span class="num" role="columnheader">BPM</span>
        <span class="key" role="columnheader">Key</span>
        <span class="len" role="columnheader">Length</span>
        <span class="tags" role="columnheader">Tags</span>
      </div>
      {#each entries as entry (entry.path)}
        <div
          class="row"
          class:current={entry.path === current}
          class:playing={entry.path === playing}
          class:broken={entry.error !== null}
          role="row"
          tabindex="0"
          title={entryTitle(entry)}
          onclick={() => audition(entry)}
          ondblclick={() => add(entry)}
          onkeydown={(event) => {
            if (event.key === "Enter") void add(entry);
            else if (event.key === " ") {
              event.preventDefault();
              void audition(entry);
            }
          }}
        >
          <span class="name" role="cell">{entry.name}</span>
          <span class="num" role="cell" class:detected={entry.bpmDetected}>{bpmLabel(entry.bpm)}</span>
          <span class="key" role="cell">{keyLabel(entry.key)}</span>
          <span class="len" role="cell">{durationLabel(entry.durationSeconds)}</span>
          <span class="tags" role="cell">{entry.error ?? entry.tags.join(" ")}</span>
        </div>
      {/each}
    {/if}
  </div>

  <footer>
    <button type="button" onclick={() => add(selected)} disabled={!selected || selected.error !== null || adding}>
      Add to {selectedTrack === null ? "a new track" : "the selected track"}
    </button>
    <button type="button" onclick={stopPreview} disabled={playing === null}>Stop preview</button>
    <span class="hint">
      Click plays a file in the project tempo; double-click adds it at the playhead. Loops of up to 32
      bars follow the project tempo.
    </span>
  </footer>
</section>

<style>
  .browser {
    display: flex;
    flex-direction: column;
    height: 260px;
    flex: none;
    border-top: 1px solid var(--border);
    background: var(--panel);
    min-height: 0;
  }
  header,
  footer,
  .status {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 8px;
  }
  .status {
    color: var(--muted);
    padding-top: 0;
  }
  .spacer {
    flex: 1;
  }
  .text {
    width: 200px;
  }
  input.num {
    width: 56px;
  }
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    border-top: 1px solid var(--border);
  }
  .row {
    display: grid;
    grid-template-columns: minmax(120px, 2fr) 56px 44px 52px minmax(80px, 3fr);
    gap: 8px;
    padding: 2px 8px;
    cursor: default;
    align-items: center;
  }
  .row:not(.head):hover {
    background: var(--panel-2);
  }
  .row.current {
    background: var(--panel-2);
    outline: 1px solid var(--border);
  }
  .row.playing {
    color: var(--accent);
  }
  .row.broken {
    color: var(--muted);
  }
  .head {
    position: sticky;
    top: 0;
    background: var(--panel);
    color: var(--muted);
    border-bottom: 1px solid var(--border);
  }
  .row span {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .row .num,
  .row .len {
    text-align: right;
  }
  .num.detected {
    font-style: italic;
  }
  .tags {
    color: var(--muted);
  }
  .empty,
  .hint {
    color: var(--muted);
    margin: 8px;
  }
  .hint {
    margin: 0;
  }
</style>
