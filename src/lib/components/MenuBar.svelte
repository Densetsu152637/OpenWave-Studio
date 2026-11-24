<script lang="ts">
  import type { FileActions } from "../arrange/files";
  import UpdateControl from "./UpdateControl.svelte";

  let {
    onSettings,
    onUpdates,
    file,
    title,
    hasProject,
    onError,
  }: {
    onSettings: () => void;
    /** Opens Settings at the Updates section. */
    onUpdates: () => void;
    file: FileActions;
    /** The project's name, with a mark when it has unsaved changes. */
    title: string;
    hasProject: boolean;
    onError: (message: string) => void;
  } = $props();

  const menus = ["Edit", "View", "Track", "Help"];
  const items: { label: string; run: () => Promise<boolean>; needsProject?: boolean }[] = [
    { label: "New", run: () => file.newProject() },
    { label: "Open…", run: () => file.openProject() },
    { label: "Save", run: () => file.save(), needsProject: true },
    { label: "Save As…", run: () => file.saveAs(), needsProject: true },
    { label: "Export WAV…", run: () => file.exportWav(), needsProject: true },
    { label: "Export WAV, normalised to -14 LUFS…", run: () => file.exportNormalized(), needsProject: true },
  ];

  let open = $state(false);

  async function choose(run: () => Promise<boolean>) {
    open = false;
    try {
      await run();
    } catch (error) {
      onError(String(error));
    }
  }

  function onWindowPointer(event: PointerEvent) {
    if (open && !(event.target as HTMLElement).closest(".file-menu")) open = false;
  }
</script>

<svelte:window onpointerdown={onWindowPointer} onkeydown={(event) => event.key === "Escape" && (open = false)} />

<nav class="menubar" aria-label="Main menu">
  <span class="brand">OpenWave Studio</span>
  <div class="file-menu">
    <button
      type="button"
      class="menu"
      aria-haspopup="menu"
      aria-expanded={open}
      onclick={() => (open = !open)}
    >
      File
    </button>
    {#if open}
      <div class="dropdown" role="menu">
        {#each items as item (item.label)}
          <button
            type="button"
            class="item"
            role="menuitem"
            disabled={item.needsProject && !hasProject}
            onclick={() => choose(item.run)}
          >
            {item.label}
          </button>
        {/each}
      </div>
    {/if}
  </div>
  {#each menus as menu (menu)}
    <button type="button" class="menu">{menu}</button>
  {/each}
  <span class="title" title="Project">{title}</span>
  <span class="end">
    <UpdateControl onOpen={onUpdates} />
    <button type="button" class="menu" onclick={onSettings}>Settings</button>
  </span>
</nav>

<style>
  .menubar {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 2px 8px;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
  }
  .brand {
    font-weight: 600;
    margin-right: 12px;
  }
  .title {
    margin-left: 16px;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .end {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .menu {
    background: transparent;
    border-color: transparent;
    padding: 3px 10px;
  }
  .file-menu {
    position: relative;
  }
  .dropdown {
    position: absolute;
    top: 100%;
    left: 0;
    z-index: 10;
    display: flex;
    flex-direction: column;
    min-width: 150px;
    padding: 4px 0;
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-radius: 4px;
  }
  .item {
    text-align: left;
    background: transparent;
    border: none;
    border-radius: 0;
    padding: 4px 14px;
  }
  .item:hover:not(:disabled) {
    background: var(--accent);
    color: var(--bg);
  }
</style>
