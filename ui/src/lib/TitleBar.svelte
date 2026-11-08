<script lang="ts">
  import { onMount } from 'svelte';
  import { onWindowState, windowCommands } from './ipc';

  let maximized = $state(false);
  let error = $state('');

  function reportError(reason: unknown) {
    error = reason instanceof Error ? reason.message : String(reason);
  }

  function control(command: () => Promise<unknown>) {
    error = '';
    void command().catch(reportError);
  }

  onMount(() => {
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    void onWindowState((event) => {
      maximized = event.payload.maximized;
    })
      .then(async (stop) => {
        if (disposed) {
          stop();
          return;
        }
        unsubscribe = stop;
        const state = await windowCommands.getState();
        if (!disposed) maximized = state.maximized;
      })
      .catch(reportError);
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  });
</script>

<header class="window-titlebar">
  <div class="brand" data-tauri-drag-region>
    <b data-tauri-drag-region>Velocity</b>
  </div>
  <div class="titlebar-space" data-tauri-drag-region></div>
  <div class="window-controls" aria-label="Window controls">
    <button
      aria-label="Minimize"
      title="Minimize"
      onclick={() => control(windowCommands.minimize)}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 8h10" /></svg>
    </button>
    <button
      aria-label={maximized ? 'Restore' : 'Maximize'}
      title={maximized ? 'Restore' : 'Maximize'}
      onclick={() => control(windowCommands.toggleMaximize)}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true">
        {#if maximized}<path d="M5.5 3.5h7v7M3.5 5.5h7v7h-7z" />{:else}<rect
            x="3.5"
            y="3.5"
            width="9"
            height="9"
            rx="1"
          />{/if}
      </svg>
    </button>
    <button
      class="close"
      aria-label="Close"
      title="Close"
      onclick={() => control(windowCommands.close)}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true"
        ><path d="M4 4l8 8M12 4l-8 8" /></svg
      >
    </button>
  </div>
</header>
{#if error}<p class="window-error" role="alert">{error}</p>{/if}
