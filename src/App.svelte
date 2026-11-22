<script lang="ts">
  import { onMount } from "svelte";
  import MenuBar from "./lib/components/MenuBar.svelte";
  import Arrangement from "./lib/components/Arrangement.svelte";
  import Transport from "./lib/components/Transport.svelte";
  import StatusBar from "./lib/components/StatusBar.svelte";
  import SettingsDialog from "./routes/settings/SettingsDialog.svelte";
  import { engineStatus, offlineStatus, prefsGet, setTestTone, type EngineStatus } from "./lib/ipc";
  import { applyTheme } from "./lib/prefs";
  import { modelMirror, type MirrorState } from "./lib/stores";

  const POLL_MS = 500;

  let status = $state<EngineStatus>(offlineStatus);
  let settingsOpen = $state(false);
  let mirror = $state<MirrorState>(modelMirror.state);

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
    // The saved theme applies at launch, not only when Settings opens.
    prefsGet()
      .then((snapshot) => applyTheme(snapshot.prefs.appearance.theme))
      .catch(() => {});
    const timer = setInterval(refresh, POLL_MS);
    return () => {
      clearInterval(timer);
      stopMirror();
    };
  });
</script>

<div class="shell">
  <MenuBar onSettings={() => (settingsOpen = true)} />
  <Transport {status} onToggle={toggleTone} />
  <Arrangement />
  <StatusBar {status} engineError={mirror.history?.engineError ?? null} />
</div>

{#if settingsOpen}
  <SettingsDialog {status} onStatus={(next) => (status = next)} onClose={() => (settingsOpen = false)} />
{/if}

<style>
  .shell {
    display: grid;
    grid-template-rows: auto auto 1fr auto;
    height: 100%;
  }
</style>
