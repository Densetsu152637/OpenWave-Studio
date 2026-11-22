<script lang="ts">
  import type { Preferences } from "../../lib/ipc";

  let { prefs = $bindable() }: { prefs: Preferences } = $props();

  let newPluginDir = $state("");

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
  <p class="hint">Paths must be absolute. They take effect when the project and plugin features read them.</p>
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
</style>
