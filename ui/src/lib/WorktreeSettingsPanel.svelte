<script lang="ts">
  import PaneState from './PaneState.svelte';
  import type {
    LayoutAction,
    WorktreeSettingsView,
    WorktreeSettingsEdit,
    WorktreePathPreview,
    MirrorState,
  } from './ipc';
  import './theme/worktrees.css';
  import SettingsNav from './SettingsNav.svelte';
  import RosenMirror from './RosenMirror.svelte';
  import MachinesPanel from './MachinesPanel.svelte';

  let {
    view,
    mirror,
    projectIdentifier,
    error,
    notice,
    pruning,
    preview,
    previewFolder,
    edit,
    browse,
    prune,
    retry,
    send,
  }: {
    view: WorktreeSettingsView | null;
    mirror: MirrorState | null;
    projectIdentifier: number | null;
    error: string;
    notice: string;
    pruning: boolean;
    preview: WorktreePathPreview | null;
    previewFolder: (folder: string) => void;
    edit: (change: WorktreeSettingsEdit) => void;
    browse: () => Promise<void>;
    prune: () => Promise<void>;
    retry: () => Promise<void>;
    send: (action: LayoutAction) => void;
  } = $props();

  function blurOnEscape(event: KeyboardEvent) {
    if (
      event.key === 'Escape' &&
      event.currentTarget instanceof HTMLInputElement
    )
      event.currentTarget.blur();
  }
</script>

<section class="worktree-settings" aria-label="Workspaces settings">
  <header class="worktree-settings-head">
    <h1>Settings</h1>
    <span>applies to every project</span>
  </header>
  <div class="worktree-settings-body">
    <SettingsNav current="worktrees" {send} />
    <div class="worktree-settings-main">
      <PaneState
        name="Workspaces"
        rosen
        network={false}
        loading={!view}
        {error}
        {retry}
      />
      {#if view}
        <section>
          <h2>Version control</h2>
          <RosenMirror
            state={view.project_identifier === projectIdentifier
              ? mirror
              : null}
          />
          <p class="worktree-description">
            Each agent gets its own workspace and change. Rosen stores edits
            automatically and lands named changes on the project base. A Rosen
            failure retries, then leaves a retryable thread error. The legacy
            GitHub workflow is used only when selected for the project.
          </p>
          <div class="worktree-checks">
            <label class="worktree-check"
              ><input
                type="radio"
                name="version-control"
                checked={view.uses_rosen}
                disabled={view.project_identifier === null}
                onchange={() => edit({ UseRosen: true })}
              />Rosen workspaces, landed on the Rosen server</label
            >
            <label class="worktree-check"
              ><input
                type="radio"
                name="version-control"
                checked={!view.uses_rosen}
                disabled={view.project_identifier === null}
                onchange={() => edit({ UseRosen: false })}
              />Legacy GitHub workflow</label
            >
          </div>
          {#if view.version_control_note}<p class="worktree-location-notice">
              {view.version_control_note}
            </p>{/if}
        </section>
        <section>
          <h2>Workspace location</h2>
          <p class="worktree-description">
            Every thread and every agent works in its own workspace, so parallel
            agents never touch each other's files. Each one is a full checkout:
            put them on a fast drive with room.
          </p>
          <div class="worktree-drives">
            {#each view.drives as drive (drive.root)}
              <button
                class="worktree-drive"
                class:on={drive.selected}
                class:slow={drive.slow}
                aria-pressed={drive.selected}
                onclick={() => edit({ Drive: drive.root })}
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
                  <span style={`--worktree-drive-used: ${drive.used_percent}%`}
                  ></span>
                </span>
                <span class="drive-free"
                  >{drive.free_label}{drive.slow
                    ? ' · slow for builds'
                    : ''}</span
                >
              </button>
            {/each}
          </div>
          <div class="worktree-location-input">
            <label class="worktree-input"
              ><span>{view.drive}</span><input
                aria-label="Workspace folder"
                value={view.folder}
                spellcheck="false"
                onkeydown={blurOnEscape}
                oninput={(event) => previewFolder(event.currentTarget.value)}
                onchange={(event) =>
                  edit({ Folder: event.currentTarget.value })}
              /></label
            >
            <button class="worktree-button browse" onclick={() => void browse()}
              >Browse…</button
            >
          </div>
          <div class="worktree-path-preview">
            {preview?.preview_root ?? view.preview_root}<span
              >&lt;project&gt;</span
            >{view.separator}<span>&lt;change&gt;</span><br /><small
              >e.g. {preview?.example_path ?? view.example_path}</small
            >
          </div>
          <div class="worktree-setting-row">
            <label class="worktree-check"
              ><input
                type="checkbox"
                checked={view.project_override !== null}
                disabled={view.project_identifier === null}
                onchange={(event) =>
                  edit({ OverrideEnabled: event.currentTarget.checked })}
              />Use a different location for <b>{view.project_name}</b></label
            >
            {#if view.project_override !== null}<label class="worktree-input"
                ><input
                  aria-label="Project workspace location"
                  value={view.project_override}
                  spellcheck="false"
                  onkeydown={blurOnEscape}
                  onchange={(event) =>
                    edit({ ProjectRoot: event.currentTarget.value })}
                /></label
              >{/if}
          </div>
          {#if view.location_notice}<p class="worktree-location-notice">
              {view.location_notice}
            </p>{/if}
        </section>
        <section>
          <h2>Lifecycle</h2>
          <p class="worktree-description">
            A workspace lives exactly as long as its work.
          </p>
          <div class="worktree-checks">
            <label class="worktree-check locked"
              ><input type="checkbox" checked disabled />Create a workspace
              whenever a thread starts or any agent starts working
              <span class="worktree-lock"
                ><svg viewBox="0 0 16 16" aria-hidden="true"
                  ><rect x="3.5" y="7" width="9" height="6.5" rx="1" /><path
                    d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2"
                  /></svg
                >always</span
              ></label
            >
            <label class="worktree-check locked"
              ><input type="checkbox" checked disabled />Delete it as soon as
              its change lands
              <span class="worktree-lock"
                ><svg viewBox="0 0 16 16" aria-hidden="true"
                  ><rect x="3.5" y="7" width="9" height="6.5" rx="1" /><path
                    d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2"
                  /></svg
                >always</span
              ></label
            >
            <label class="worktree-check"
              ><input
                type="checkbox"
                checked={view.delete_local_branch}
                onchange={(event) =>
                  edit({ DeleteLocalBranch: event.currentTarget.checked })}
              />Legacy git: delete the local branch too</label
            >
          </div>
          <div class="worktree-setting-row lifecycle-first">
            <span>Change abandoned</span><span class="hint"
              >workspace kept · right-click the thread → Delete removes both</span
            >
          </div>
          <div class="worktree-setting-row">
            <span>Thread failed</span><span class="hint"
              >workspace kept · same: delete the thread to remove it</span
            >
          </div>
          <div class="worktree-setting-row">
            <span>Spec interview</span><span class="hint"
              >no workspace · it only reads the repo and writes SPEC.md</span
            >
          </div>
        </section>
        <section>
          <h2>New workspace setup</h2>
          <p class="worktree-description">
            Runs before the agent starts, so it never works in a half-set-up
            checkout.
          </p>
          <div class="worktree-setting-row">
            <label for="worktree-copy">Copy untracked files</label><label
              class="worktree-input"
              ><input
                id="worktree-copy"
                value={view.copy_files}
                spellcheck="false"
                onkeydown={blurOnEscape}
                onchange={(event) =>
                  edit({ CopyFiles: event.currentTarget.value })}
              /></label
            >
          </div>
          <div class="worktree-setting-row">
            <label for="worktree-setup"
              >Run after creating · {view.project_name}</label
            ><label class="worktree-input"
              ><input
                id="worktree-setup"
                value={view.setup_command}
                disabled={view.project_identifier === null}
                spellcheck="false"
                onkeydown={blurOnEscape}
                onchange={(event) =>
                  edit({ SetupCommand: event.currentTarget.value })}
              /></label
            >
          </div>
          <div class="worktree-setting-row">
            <label class="worktree-check"
              ><input
                type="checkbox"
                checked={view.share_build_cache}
                onchange={(event) =>
                  edit({ ShareBuildCache: event.currentTarget.checked })}
              />Share the Rust build cache between a project's workspaces</label
            ><span class="hint"
              >{view.share_build_cache
                ? 'no cold builds · ≈2 GB less each'
                : 'each worktree builds from scratch'}</span
            >
          </div>
          <div class="worktree-setting-row">
            <label for="worktree-ready">Keep N workspaces ready</label><label
              class="worktree-input"
              ><input
                id="worktree-ready"
                type="number"
                min="0"
                step="1"
                value={view.ready_count}
                onkeydown={blurOnEscape}
                onchange={(event) =>
                  edit({ ReadyCount: event.currentTarget.valueAsNumber })}
              /></label
            >
          </div>
        </section>
        {#if view.uses_rosen && view.project_identifier !== null}
          {#key view.project_identifier}<MachinesPanel
              project={view.project_identifier}
            />{/key}
        {/if}
        <section>
          <h2>Live workspaces</h2>
          <p class="worktree-description">
            Every workspace Velocity created and hasn't deleted yet.
          </p>
          <div class="worktree-summary">
            <span>{view.live_count} live · {view.total_size}</span><span
              class="free-space">{view.free_space}</span
            ><button
              class="worktree-button mini"
              disabled={!view.can_prune || pruning}
              onclick={() => void prune()}
              >{pruning ? 'Pruning…' : 'Prune orphans'}</button
            >
          </div>
          {#each view.rows as row (row.path)}
            <button
              class="worktree-row"
              class:gone={row.deleted}
              title={row.warning ? `${row.path}\n${row.warning}` : row.path}
              onclick={() => send({ OpenThread: String(row.owner_identifier) })}
            >
              <span class="worktree-dot" data-tone={row.tone}></span><span
                class="path">{row.short_path}</span
              ><span class="owner">{row.owner}</span><span class="size"
                >{row.size}</span
              ><span class="state">{row.label}</span>
            </button>
          {/each}
          <PaneState
            name="Workspaces"
            network={false}
            empty={view.rows.length === 0}
            message="No live workspaces · Start a thread to create one."
          />
          <p class="worktree-size-note">
            Sizes exclude linked directories and shared build caches outside the
            workspace.
          </p>
        </section>
      {/if}
      {#if notice}<p class="worktree-message" role="status">{notice}</p>{/if}
    </div>
  </div>
</section>
