<script lang="ts">
  import WindowControls from './WindowControls.svelte';
  import type { LayoutAction, LayoutSnapshot, ProjectHealthItem } from './ipc';
  import ProjectSwitcher from './ProjectSwitcher.svelte';
  import ProjectHealth from './ProjectHealth.svelte';
  import RosenMirror from './RosenMirror.svelte';
  import RosenServerNotice from './RosenServerNotice.svelte';
  import Icon from './Icon.svelte';
  import LocalBuild from './LocalBuild.svelte';

  let {
    layout,
    send,
    switchProject,
  }: {
    layout: LayoutSnapshot | null;
    send: (action: LayoutAction) => void;
    switchProject: (identifier: number) => void;
  } = $props();

  let collapseForwarding = $state<ProjectHealthItem | null>(null);
  const activeCollapseForwarding = $derived(
    layout?.project_identifier != null ? collapseForwarding : null,
  );
</script>

{#snippet pulseCounts()}
  {#each layout?.pulse ?? [] as count (count.kind)}
    <span
      role="img"
      data-kind={count.kind}
      title={`${count.count} ${count.label}`}
      aria-label={`${count.count} ${count.label}`}
      >{#if count.kind !== 'MergedToday'}<span class="pulse-dot"
        ></span>{:else}↳{/if}{count.count}
      <span class="pulse-label">{count.label}</span></span
    >
  {/each}
{/snippet}

<div class="project-header">
  <header class="window-titlebar">
    <div class="brand" data-tauri-drag-region>
      <b data-tauri-drag-region>Velocity</b>
      <ProjectSwitcher
        name={layout?.project_name ?? ''}
        project={layout?.project_identifier ?? null}
        collapse={layout?.collapse_repository ?? null}
        collapseLiveState={layout?.collapse_live_state ?? false}
        rosen={layout?.rosen_work != null}
        {send}
        select={switchProject}
      />
    </div>
    <div class="titlebar-space" data-tauri-drag-region>
      <LocalBuild />
      <div class="titlebar-center" data-tauri-drag-region>
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
        {#if layout?.pulse.length}<div
            class="pulse"
            role="group"
            aria-label="Project pulse"
          >
            {@render pulseCounts()}
          </div>{/if}
      </div>
      <div class="titlebar-tools">
        <button
          class="palette-trigger"
          title="Command palette (Ctrl+K)"
          aria-label="Open command palette"
          onclick={() => send({ Request: { Navigation: 'Palette' } })}
          ><Icon name="search" /><span>Search and run commands</span><kbd
            >Ctrl</kbd
          ><kbd>K</kbd></button
        >
        {#if layout?.project_identifier != null}
          <ProjectHealth
            project={layout.project_identifier}
            {send}
            onForwardingWarning={(item) => (collapseForwarding = item)}
          />
        {/if}
      </div>
      <button
        class="btn pulse-trigger"
        popovertarget="compact-project-pulse"
        aria-label="Project pulse"
        title="Project pulse">Pulse</button
      >
    </div>
    <WindowControls />
    <div
      id="compact-project-pulse"
      class="pulse pulse-popover"
      popover
      aria-label="Project pulse"
      role="group"
    >
      {@render pulseCounts()}
      <button
        popovertarget="compact-project-pulse"
        popovertargetaction="hide"
        onclick={() => send({ Request: { Navigation: 'Palette' } })}
        >Command palette <kbd>Ctrl</kbd><kbd>K</kbd></button
      >
    </div>
  </header>
  <RosenServerNotice notice={layout?.rosen_server_notice ?? null} />
  <RosenMirror
    state={layout?.rosen_mirror ?? null}
    behind={layout?.rosen_mirror_behind ?? null}
    divergence={layout?.rosen_mirror_divergence ?? null}
    project={layout?.project_identifier ?? null}
    collapse={layout?.collapse_mirror_health ?? null}
    collapseForwarding={activeCollapseForwarding}
  />
</div>
