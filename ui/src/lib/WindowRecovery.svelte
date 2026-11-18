<script lang="ts">
  import WindowControls from './WindowControls.svelte';

  let { error = '' }: { error?: string } = $props();
</script>

<header class="window-titlebar">
  <div class="brand" data-tauri-drag-region>
    <b data-tauri-drag-region>Velocity</b>
  </div>
  <div data-tauri-drag-region></div>
  <WindowControls />
</header>
<main class="empty" aria-label="Velocity workspace">
  {#if error}
    <p role="alert">{error}</p>
    <button type="button" class="btn" onclick={() => window.location.reload()}
      >Reload</button
    >
  {:else}
    <p role="status">Reconnecting…</p>
  {/if}
</main>
