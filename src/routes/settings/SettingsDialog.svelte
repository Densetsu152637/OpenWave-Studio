<script lang="ts">
  import UpdatePanel from "./update/UpdatePanel.svelte";

  let { onClose }: { onClose: () => void } = $props();
</script>

<svelte:window onkeydown={(event) => event.key === "Escape" && onClose()} />

<div class="backdrop" role="presentation" onclick={onClose}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-label="Settings"
    tabindex="-1"
    onclick={(event) => event.stopPropagation()}
    onkeydown={(event) => event.stopPropagation()}
  >
    <header>
      <h2>Settings</h2>
      <button type="button" onclick={onClose} aria-label="Close settings">Close</button>
    </header>
    <UpdatePanel />
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: rgba(0, 0, 0, 0.45);
    z-index: 10;
  }
  .dialog {
    width: min(520px, 90vw);
    max-height: 80vh;
    overflow: auto;
    padding: 12px 18px 18px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    user-select: text;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  h2 {
    margin: 0;
    font-size: 1.2em;
  }
</style>
