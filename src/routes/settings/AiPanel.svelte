<script lang="ts">
  import type { Preferences } from "../../lib/ipc";

  let { prefs = $bindable() }: { prefs: Preferences } = $props();
</script>

<section aria-labelledby="ai-heading">
  <h3 id="ai-heading">AI control</h3>
  <label>
    <input type="checkbox" bind:checked={prefs.ai.allowControl} />
    Allow AI control (MCP)
  </label>
  <p class="hint">
    Lets an AI agent that you start (for example Claude Code with <code>openwave-studio mcp</code>) edit the
    open project, mix, analyse and export through a local connection that only your user account can open. It has
    your file access for imports, saves and exports. Nothing is reachable over the network.
  </p>
</section>

<style>
  .hint {
    margin: 8px 0 0;
    max-width: 60ch;
    color: var(--muted);
  }
</style>
