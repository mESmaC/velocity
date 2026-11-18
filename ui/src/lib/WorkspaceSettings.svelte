<script lang="ts">
  import { onMount } from 'svelte';
  import PaneState from './PaneState.svelte';
  import SettingsNav from './SettingsNav.svelte';
  import WorkspaceLocation from './WorkspaceLocation.svelte';
  import RosenWorkspaces from './RosenWorkspaces.svelte';
  import RosenMirror from './RosenMirror.svelte';
  import {
    workspaceCommands,
    type LayoutAction,
    type MirrorState,
    type WorkspacePreferences,
    type WorktreeSettingsEdit,
  } from './ipc';
  import './theme/worktrees.css';

  let {
    send,
    mirror,
    projectIdentifier,
  }: {
    send: (action: LayoutAction) => void;
    mirror: MirrorState | null;
    projectIdentifier: number | null;
  } = $props();

  let view = $state<WorkspacePreferences | null>(null);
  let error = $state('');
  let queue = Promise.resolve();

  async function refresh() {
    try {
      const preferences = await workspaceCommands.settings();
      if (preferences.project_identifier === projectIdentifier)
        view = preferences;
      error = '';
    } catch (reason) {
      error = String(reason);
    }
  }

  function edit(change: WorktreeSettingsEdit) {
    const project = projectIdentifier;
    queue = queue.then(async () => {
      try {
        const preferences = await workspaceCommands.set(change, project);
        if (preferences.project_identifier === projectIdentifier)
          view = preferences;
        error = '';
      } catch (reason) {
        error = String(reason);
      }
    });
  }

  onMount(() => void refresh());
</script>

<section class="worktree-settings" aria-label="Workspaces settings">
  <header class="worktree-settings-head">
    <h1>Settings</h1>
  </header>
  <div class="worktree-settings-body">
    <SettingsNav current="worktrees" {send} />
    <div class="worktree-settings-main">
      <PaneState
        name="Workspace preferences"
        network={false}
        loading={!view}
        {error}
        retry={refresh}
      />
      {#if view}
        <RosenMirror state={mirror} />
        <WorkspaceLocation
          {view}
          {edit}
          onError={(reason) => (error = String(reason))}
        />
        <section>
          <h2>Agent setup</h2>
          <div class="worktree-setting-row">
            <label for="workspace-copy">Copy untracked files</label>
            <label class="worktree-input">
              <input
                id="workspace-copy"
                value={view.copy_files}
                spellcheck="false"
                onchange={(event) =>
                  edit({ CopyFiles: event.currentTarget.value })}
              />
            </label>
          </div>
          {#if projectIdentifier !== null}
            <div class="worktree-setting-row">
              <label for="workspace-setup">After-take setup command</label>
              <label class="worktree-input">
                <input
                  id="workspace-setup"
                  value={view.setup_command}
                  spellcheck="false"
                  onchange={(event) =>
                    edit({ SetupCommand: event.currentTarget.value })}
                />
              </label>
            </div>
          {/if}
          <div class="worktree-setting-row">
            <label class="worktree-check">
              <input
                type="checkbox"
                checked={view.share_build_cache}
                onchange={(event) =>
                  edit({ ShareBuildCache: event.currentTarget.checked })}
              />
              Share the Rust build cache between this project's workspaces
            </label>
          </div>
          <div class="worktree-setting-row">
            <span>Skills linked for each agent</span>
            <button
              class="worktree-button mini"
              onclick={() => send({ Request: { SettingsTab: 'skills' } })}
              >Manage skills</button
            >
          </div>
        </section>
        {#if projectIdentifier !== null}
          <RosenWorkspaces project={projectIdentifier} {send} />
        {/if}
      {/if}
    </div>
  </div>
</section>
