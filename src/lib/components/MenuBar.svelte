<script lang="ts">
  let { onSettings }: { onSettings: () => void } = $props();
  const menus = ["File", "Edit", "View", "Track", "Help"];
</script>

<nav class="menubar" aria-label="Main menu">
  <span class="brand">OpenWave Studio</span>
  {#each menus as menu (menu)}
    <button type="button" class="menu">{menu}</button>
  {/each}
  <button type="button" class="menu settings" onclick={onSettings}>Settings</button>
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
  .settings {
    margin-left: auto;
  }
  .menu {
    background: transparent;
    border-color: transparent;
    padding: 3px 10px;
  }
</style>
