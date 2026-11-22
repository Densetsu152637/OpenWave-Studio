<script lang="ts">
  import type { Preferences, Theme } from "../../lib/ipc";
  import { applyTheme } from "../../lib/prefs";

  let { prefs = $bindable() }: { prefs: Preferences } = $props();

  const themes: { value: Theme; label: string }[] = [
    { value: "system", label: "Follow the system" },
    { value: "light", label: "Light" },
    { value: "dark", label: "Dark" },
  ];

  function pick(theme: Theme) {
    prefs.appearance.theme = theme;
    // Preview straight away; Revert or Close without Apply puts the saved theme back.
    applyTheme(theme);
  }
</script>

<section aria-labelledby="appearance-heading">
  <h3 id="appearance-heading">Theme</h3>
  <div role="radiogroup" aria-labelledby="appearance-heading">
    {#each themes as theme (theme.value)}
      <label>
        <input
          type="radio"
          name="theme"
          value={theme.value}
          checked={prefs.appearance.theme === theme.value}
          onchange={() => pick(theme.value)}
        />
        {theme.label}
      </label>
    {/each}
  </div>
</section>

<style>
  div {
    display: grid;
    gap: 6px;
  }
</style>
