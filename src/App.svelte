<script lang="ts">
  import { onMount } from "svelte";
  import MenuBar from "./lib/components/MenuBar.svelte";
  import Arrangement from "./lib/components/Arrangement.svelte";
  import Transport from "./lib/components/Transport.svelte";
  import StatusBar from "./lib/components/StatusBar.svelte";
  import SettingsDialog from "./routes/settings/SettingsDialog.svelte";
  import { engineStatus, offlineStatus, setTestTone, type EngineStatus } from "./lib/ipc";

  const POLL_MS = 500;

  let status = $state<EngineStatus>(offlineStatus);
  let settingsOpen = $state(false);

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
    const timer = setInterval(refresh, POLL_MS);
    return () => clearInterval(timer);
  });
</script>

<div class="shell">
  <MenuBar onSettings={() => (settingsOpen = true)} />
  <Transport {status} onToggle={toggleTone} />
  <Arrangement />
  <StatusBar {status} />
</div>

{#if settingsOpen}
  <SettingsDialog onClose={() => (settingsOpen = false)} />
{/if}

<style>
  .shell {
    display: grid;
    grid-template-rows: auto auto 1fr auto;
    height: 100%;
  }
</style>
