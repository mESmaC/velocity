<script lang="ts">
  import { onMount } from 'svelte';
  import PaneState from './PaneState.svelte';
  import SettingsNav from './SettingsNav.svelte';
  import WorkspaceLocation from './WorkspaceLocation.svelte';
  import RosenWorkspaces from './RosenWorkspaces.svelte';
  import RosenMirror from './RosenMirror.svelte';
  import CollapseProjectSettings from './CollapseProjectSettings.svelte';
  import DeploySecretsSettings from './DeploySecretsSettings.svelte';
  import {
    projectCommands,
    workspaceCommands,
    type LayoutAction,
    type MirrorState,
    type ProjectGitHubRepository,
    type WorkspacePreferences,
    type WorkspaceSettingsEdit,
  } from './ipc';
  import './theme/workspaces.css';

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
  let githubView = $state<ProjectGitHubRepository | null>(null);
  let githubInput = $state('');
  let githubSaving = $state(false);
  let githubError = $state('');
  let githubSaved = $state(false);
  let queue = Promise.resolve();

  const suggestedDownstream = $derived(
    githubView?.suggested_downstream &&
      githubView.suggested_downstream.toLowerCase() !==
        (githubView.repository ?? '').toLowerCase()
      ? githubView.suggested_downstream
      : null,
  );

  async function refreshGitHub() {
    if (projectIdentifier === null) {
      githubView = null;
      githubInput = '';
      return;
    }
    const target = projectIdentifier;
    try {
      const loaded = await projectCommands.githubRepository(target);
      if (loaded.project_identifier === projectIdentifier) {
        githubView = loaded;
        githubInput = loaded.repository ?? '';
        githubError = '';
      }
    } catch (reason) {
      if (target === projectIdentifier) githubError = String(reason);
    }
  }

  async function saveGitHub(value: string) {
    if (projectIdentifier === null || githubSaving) return;
    const target = projectIdentifier;
    githubSaving = true;
    githubError = '';
    githubSaved = false;
    try {
      const updated = await projectCommands.setGitHubRepository(target, value);
      if (updated.project_identifier === projectIdentifier) {
        githubView = updated;
        githubInput = updated.repository ?? '';
        githubSaved = true;
      }
    } catch (reason) {
      if (target === projectIdentifier) githubError = String(reason);
    } finally {
      githubSaving = false;
    }
  }

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

  function edit(change: WorkspaceSettingsEdit) {
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

  onMount(() => {
    void refresh();
    void refreshGitHub();
  });
</script>

<section class="workspace-settings" aria-label="Workspaces settings">
  <header class="workspace-settings-head">
    <h1>Settings</h1>
  </header>
  <div class="workspace-settings-body">
    <SettingsNav current="workspaces" {send} />
    <div class="workspace-settings-main">
      <PaneState
        name="Workspace preferences"
        network={false}
        loading={!view}
        {error}
        retry={refresh}
      />
      {#if view}
        <RosenMirror state={mirror} />
        {#if projectIdentifier !== null}
          <CollapseProjectSettings project={projectIdentifier} />
          <DeploySecretsSettings project={projectIdentifier} />
        {/if}
        {#if projectIdentifier !== null && githubView}
          <section>
            <h2>Issues and CI on GitHub</h2>
            <p class="workspace-description">
              GitHub repository used for continuous dispatch, the issues rail,
              base CI, releases and Rosen pin bumps.
            </p>
            <form
              class="workspace-location-input"
              onsubmit={(event) => {
                event.preventDefault();
                void saveGitHub(githubInput);
              }}
            >
              <label class="workspace-input">
                <span>github.com/</span>
                <input
                  aria-label="Issues and CI on GitHub"
                  placeholder={suggestedDownstream ?? 'owner/repo'}
                  spellcheck="false"
                  disabled={githubSaving}
                  bind:value={githubInput}
                  oninput={() => (githubSaved = false)}
                />
              </label>
              <button
                type="submit"
                class="workspace-button browse"
                disabled={githubSaving}
                >{githubSaving ? 'Saving…' : 'Save'}</button
              >
            </form>
            {#if suggestedDownstream}
              <div class="workspace-github-suggestion">
                <span
                  >Collapse forwards to <code>{suggestedDownstream}</code></span
                >
                <button
                  type="button"
                  class="workspace-button mini"
                  disabled={githubSaving}
                  onclick={() => void saveGitHub(suggestedDownstream)}
                  >Use {suggestedDownstream}</button
                >
              </div>
            {/if}
            {#if githubError}
              <p class="workspace-message error" role="alert">{githubError}</p>
            {:else if githubSaved}
              <p class="workspace-message" role="status">
                {githubView.repository
                  ? `Issues and CI linked to ${githubView.repository}.`
                  : 'GitHub repository cleared.'}
              </p>
            {/if}
          </section>
        {/if}
        <WorkspaceLocation
          {view}
          {edit}
          onError={(reason) => (error = String(reason))}
        />
        <section>
          <h2>Agent setup</h2>
          <div class="workspace-setting-row">
            <label for="workspace-copy">Copy untracked files</label>
            <label class="workspace-input">
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
            <div class="workspace-setting-row">
              <label for="workspace-setup">After-take setup command</label>
              <label class="workspace-input">
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
          <div class="workspace-setting-row">
            <label class="workspace-check">
              <input
                type="checkbox"
                checked={view.share_build_cache}
                onchange={(event) =>
                  edit({ ShareBuildCache: event.currentTarget.checked })}
              />
              Share the Rust build cache between this project's workspaces
            </label>
          </div>
          <div class="workspace-setting-row">
            <span>Skills linked for each agent</span>
            <button
              class="workspace-button mini"
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
