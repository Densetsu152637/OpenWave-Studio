<script lang="ts">
  import type { Preferences } from "../../lib/ipc";
  import { pluginsList, pluginsScan } from "../../lib/ipc/commands";
  import type { PluginScanSummary } from "../../lib/ipc/types";
  import { onMount } from "svelte";

  let { prefs = $bindable() }: { prefs: Preferences } = $props();

  let newPluginDir = $state("");
  let newLibraryDir = $state("");
  let scan = $state<PluginScanSummary | null>(null);
  let scanning = $state(false);
  let scanError = $state<string | null>(null);

  onMount(() => {
    pluginsList()
      .then((summary) => (scan = summary))
      .catch(() => {});
  });

  async function scanNow(force: boolean) {
    scanning = true;
    scanError = null;
    try {
      scan = await pluginsScan(force);
    } catch (error) {
      scanError = String(error);
    } finally {
      scanning = false;
    }
  }

  function addLibraryDir() {
    const dir = newLibraryDir.trim();
    if (dir && !prefs.paths.libraryDirs.includes(dir)) {
      prefs.paths.libraryDirs = [...prefs.paths.libraryDirs, dir];
    }
    newLibraryDir = "";
  }

  function addPluginDir() {
    const dir = newPluginDir.trim();
    if (dir && !prefs.paths.pluginDirs.includes(dir)) {
      prefs.paths.pluginDirs = [...prefs.paths.pluginDirs, dir];
    }
    newPluginDir = "";
  }
</script>

<section aria-labelledby="paths-heading">
  <h3 id="paths-heading">Folders</h3>

  <label for="projects-dir">Projects folder</label>
  <div class="row">
    <input
      id="projects-dir"
      type="text"
      placeholder="Default (Documents/OpenWave Studio)"
      value={prefs.paths.projectsDir ?? ""}
      oninput={(event) => (prefs.paths.projectsDir = event.currentTarget.value.trim() || null)}
    />
    <button type="button" onclick={() => (prefs.paths.projectsDir = null)} disabled={prefs.paths.projectsDir === null}>
      Use default
    </button>
  </div>

  <h4>Loop and sample folders</h4>
  <ul>
    {#each prefs.paths.libraryDirs as dir (dir)}
      <li>
        <span class="path">{dir}</span>
        <button
          type="button"
          onclick={() => (prefs.paths.libraryDirs = prefs.paths.libraryDirs.filter((entry) => entry !== dir))}
        >
          Remove
        </button>
      </li>
    {:else}
      <li class="hint">None. Add folders of loops and samples for the loop browser.</li>
    {/each}
  </ul>
  <div class="row">
    <input
      type="text"
      aria-label="Library folder to add"
      placeholder="Absolute path of a folder"
      bind:value={newLibraryDir}
      onkeydown={(event) => event.key === "Enter" && addLibraryDir()}
    />
    <button type="button" onclick={addLibraryDir} disabled={!newLibraryDir.trim()}>Add</button>
  </div>
  <p class="hint">Save the settings, then press Scan in the loop browser.</p>

  <h4>Extra plugin folders</h4>
  <ul>
    {#each prefs.paths.pluginDirs as dir (dir)}
      <li>
        <span class="path">{dir}</span>
        <button
          type="button"
          onclick={() => (prefs.paths.pluginDirs = prefs.paths.pluginDirs.filter((entry) => entry !== dir))}
        >
          Remove
        </button>
      </li>
    {:else}
      <li class="hint">None. The standard plugin locations are always scanned.</li>
    {/each}
  </ul>
  <div class="row">
    <input
      type="text"
      aria-label="Plugin folder to add"
      placeholder="Absolute path of a folder"
      bind:value={newPluginDir}
      onkeydown={(event) => event.key === "Enter" && addPluginDir()}
    />
    <button type="button" onclick={addPluginDir} disabled={!newPluginDir.trim()}>Add</button>
  </div>
  <p class="hint">Paths must be absolute. Save the settings, then scan.</p>

  <h4>Plugins</h4>
  <div class="row">
    <button type="button" onclick={() => scanNow(false)} disabled={scanning}>
      {scanning ? "Scanning\u2026" : "Scan for plugins"}
    </button>
    <button type="button" onclick={() => scanNow(true)} disabled={scanning} title="Load every plugin file again">
      Rescan all
    </button>
  </div>
  {#if scanError}
    <p class="error" role="alert">{scanError}</p>
  {/if}
  {#if scan}
    <p class="hint">
      {scan.plugins.length} usable plugin{scan.plugins.length === 1 ? "" : "s"} in {scan.files} file{scan.files ===
      1
        ? ""
        : "s"}.
    </p>
    {#if scan.failures.length > 0}
      <p class="hint">Skipped, because they crashed the scanner, did not answer or could not be loaded:</p>
      <ul>
        {#each scan.failures as failure (failure.path)}
          <li><span class="path">{failure.path}</span> <span class="hint">({failure.status})</span></li>
        {/each}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .row {
    display: flex;
    gap: 8px;
    margin: 4px 0 8px;
  }
  input[type="text"] {
    flex: 1;
    font: inherit;
    color: inherit;
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 3px 6px;
    user-select: text;
  }
  ul {
    list-style: none;
    padding: 0;
    margin: 0 0 8px;
    display: grid;
    gap: 4px;
  }
  li {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    align-items: center;
  }
  .path {
    overflow-wrap: anywhere;
  }
  h4 {
    margin: 16px 0 6px;
  }
  .hint {
    color: var(--muted);
  }
  .error {
    color: var(--danger, #c0392b);
  }
</style>
