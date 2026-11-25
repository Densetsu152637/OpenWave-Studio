<script lang="ts">
  import type { Preferences } from "../../lib/ipc";
  import {
    SHORTCUT_ACTIONS,
    acceleratorFromEvent,
    effectiveShortcut,
    shortcutConflicts,
    withShortcut,
    type ShortcutAction,
  } from "../../lib/prefs";

  let { prefs = $bindable() }: { prefs: Preferences } = $props();

  let capturing = $state<string | null>(null);

  function set(action: ShortcutAction, accelerator: string) {
    prefs = withShortcut(prefs, action, accelerator);
  }

  function capture(event: KeyboardEvent, action: ShortcutAction) {
    event.preventDefault();
    event.stopPropagation();
    if (event.key === "Escape") {
      capturing = null;
      return;
    }
    const accelerator = acceleratorFromEvent(event);
    if (accelerator === null) return;
    set(action, accelerator);
    capturing = null;
  }
</script>

<section aria-labelledby="shortcuts-heading">
  <h3 id="shortcuts-heading">Keyboard shortcuts</h3>
  <table>
    <thead>
      <tr><th>Action</th><th>Shortcut</th><th></th></tr>
    </thead>
    <tbody>
      {#each SHORTCUT_ACTIONS as action (action.id)}
        {@const accelerator = effectiveShortcut(prefs, action)}
        {@const conflicts = shortcutConflicts(prefs, action.id, accelerator)}
        <tr>
          <td>{action.label}</td>
          <td>
            {#if action.fixed}
              <kbd>{accelerator}</kbd> <span class="hint">fixed</span>
            {:else if capturing === action.id}
              <!-- svelte-ignore a11y_autofocus -->
              <button
                type="button"
                class="capture"
                autofocus
                onkeydown={(event) => capture(event, action)}
                onblur={() => (capturing = null)}
              >
                Press a key combination (Esc cancels)
              </button>
            {:else}
              <button type="button" onclick={() => (capturing = action.id)} aria-label={`Change shortcut for ${action.label}`}>
                {accelerator || "Unassigned"}
              </button>
            {/if}
            {#if conflicts.length > 0}
              <span class="warn">Also used by {conflicts.map((entry) => entry.label).join(", ")}</span>
            {/if}
          </td>
          <td class="actions">
            {#if !action.fixed}
              <button type="button" onclick={() => set(action, "")} disabled={accelerator === ""}>Clear</button>
              <button type="button" onclick={() => set(action, action.default)} disabled={accelerator === action.default}>
                Reset
              </button>
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
  <p class="hint">Shortcuts are stored now and take effect as each feature adopts them.</p>
</section>

<style>
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th,
  td {
    text-align: left;
    padding: 4px 6px;
    border-bottom: 1px solid var(--border);
  }
  .actions {
    white-space: nowrap;
    text-align: right;
  }
  .capture {
    border-color: var(--accent);
  }
  .warn {
    color: var(--danger);
    margin-left: 8px;
  }
  .hint {
    color: var(--muted);
  }
</style>
