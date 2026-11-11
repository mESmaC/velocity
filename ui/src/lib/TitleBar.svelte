<script lang="ts">
  import { onMount } from 'svelte';
  import { onWindowState, windowCommands } from './ipc';
  import type { LayoutAction, LayoutSnapshot } from './ipc';
  import ProjectSwitcher from './ProjectSwitcher.svelte';

  let {
    layout,
    send,
    switchProject,
  }: {
    layout: LayoutSnapshot | null;
    send: (action: LayoutAction) => void;
    switchProject: (identifier: number) => void;
  } = $props();

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
        const layout = await windowCommands.getState();
        if (!disposed) maximized = layout.maximized;
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
    <ProjectSwitcher
      name={layout?.project_name ?? ''}
      {send}
      select={switchProject}
    />
  </div>
  <div class="titlebar-space" data-tauri-drag-region>
    {#if layout && layout.project_identifier !== null}<div
        class="view-switch"
        role="group"
        aria-label="View"
      >
        <button
          class:on={layout.view !== 'Board'}
          aria-pressed={layout.view !== 'Board'}
          title="Focus: one agent in depth"
          onclick={() => send({ Request: { Navigation: 'Focus' } })}
          >Focus</button
        >
        <button
          class:on={layout.view === 'Board'}
          aria-pressed={layout.view === 'Board'}
          title="Board: every agent at once (B)"
          onclick={() => {
            if (layout.view !== 'Board')
              send({ Request: { Navigation: 'Board' } });
          }}>Board</button
        >
      </div>{/if}
    <div class="pulse" aria-label="Project pulse">
      {#each layout?.pulse ?? [] as count (count.kind)}<span
          data-kind={count.kind}
          >{#if count.kind !== 'MergedToday'}<span class="pulse-dot"
            ></span>{:else}↳{/if}{count.count}
          {count.label}</span
        >{/each}
      <button
        title="Command palette (Ctrl+K)"
        aria-label="Open command palette"
        onclick={() => send({ Request: { Navigation: 'Palette' } })}
        ><kbd>Ctrl</kbd><kbd>K</kbd></button
      >
    </div>
  </div>
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
