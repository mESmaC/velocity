<script lang="ts">
  import {
    workspaceCommands,
    type WorkspacePreferences,
    type WorkspacePathPreview,
    type WorkspaceSettingsEdit,
  } from './ipc';

  let {
    view,
    edit,
    onError,
  }: {
    view: WorkspacePreferences;
    edit: (change: WorkspaceSettingsEdit) => void;
    onError: (reason: unknown) => void;
  } = $props();
  let preview = $state<WorkspacePathPreview | null>(null);
  let previewRequest = 0;

  async function previewFolder(folder: string) {
    const request = ++previewRequest;
    try {
      const result = await workspaceCommands.preview(folder);
      if (request === previewRequest) preview = result;
    } catch (reason) {
      if (request === previewRequest) onError(reason);
    }
  }

  function save(change: WorkspaceSettingsEdit) {
    previewRequest += 1;
    preview = null;
    edit(change);
  }

  async function browse() {
    try {
      const path = await workspaceCommands.browse();
      if (path !== null) save({ Root: path });
    } catch (reason) {
      onError(reason);
    }
  }

  function blurOnEscape(event: KeyboardEvent) {
    if (
      event.key === 'Escape' &&
      event.currentTarget instanceof HTMLInputElement
    )
      event.currentTarget.blur();
  }
</script>

<section>
  <h2>Workspace location</h2>
  <p class="workspace-description">Location changes apply to new workspaces.</p>
  <div class="workspace-drives">
    {#each view.drives as drive (drive.root)}
      <button
        class="workspace-drive"
        class:on={drive.selected}
        class:slow={drive.slow}
        aria-pressed={drive.selected}
        onclick={() => save({ Drive: drive.root })}
      >
        <span class="drive-heading"
          >{drive.root} {drive.name}<span>{drive.kind}</span></span
        >
        <span
          class="drive-bar"
          role="meter"
          aria-label={`${drive.root} used space`}
          aria-valuemin="0"
          aria-valuemax="100"
          aria-valuenow={drive.used_percent}
        >
          <span style={`--workspace-drive-used: ${drive.used_percent}%`}></span>
        </span>
        <span class="drive-free"
          >{drive.free_label}{drive.slow ? ' · slow for builds' : ''}</span
        >
      </button>
    {/each}
  </div>
  <div class="workspace-location-input">
    <label class="workspace-input">
      <span>{view.drive}</span>
      <input
        aria-label="Workspace folder"
        value={view.folder}
        spellcheck="false"
        onkeydown={blurOnEscape}
        oninput={(event) => void previewFolder(event.currentTarget.value)}
        onchange={(event) => save({ Folder: event.currentTarget.value })}
      />
    </label>
    <button class="workspace-button browse" onclick={() => void browse()}
      >Browse…</button
    >
  </div>
  <div class="workspace-path-preview">
    {preview?.preview_root ?? view.preview_root}<span>&lt;project&gt;</span
    >{view.separator}<span>&lt;change&gt;</span>
    <br /><small>e.g. {preview?.example_path ?? view.example_path}</small>
  </div>
  {#if view.project_identifier !== null}
    <div class="workspace-setting-row">
      <label class="workspace-check">
        <input
          type="checkbox"
          checked={view.project_override !== null}
          onchange={(event) =>
            save({ OverrideEnabled: event.currentTarget.checked })}
        />
        Use a different location for <b>{view.project_name}</b>
      </label>
      {#if view.project_override !== null}
        <label class="workspace-input">
          <input
            aria-label="Project workspace location"
            value={view.project_override}
            spellcheck="false"
            onkeydown={blurOnEscape}
            onchange={(event) =>
              save({ ProjectRoot: event.currentTarget.value })}
          />
        </label>
      {/if}
    </div>
  {/if}
  {#if view.location_notice}
    <p class="workspace-location-notice">{view.location_notice}</p>
  {/if}
</section>
