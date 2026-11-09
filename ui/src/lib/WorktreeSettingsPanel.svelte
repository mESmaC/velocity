<script lang="ts">
  import type {
    LayoutAction,
    WorktreeSettingsView,
    WorktreeSettingsEdit,
  } from './ipc';
  import './theme/worktrees.css';

  let {
    view,
    error,
    notice,
    pruning,
    edit,
    browse,
    prune,
    send,
  }: {
    view: WorktreeSettingsView | null;
    error: string;
    notice: string;
    pruning: boolean;
    edit: (change: WorktreeSettingsEdit) => void;
    browse: () => Promise<void>;
    prune: () => Promise<void>;
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

<section class="worktree-settings" aria-label="Worktrees settings">
  <header class="worktree-settings-head">
    <h1>Settings</h1>
    <span>applies to every project</span>
  </header>
  <div class="worktree-settings-body">
    <nav class="worktree-settings-nav" aria-label="Settings pages">
      <button class="on" aria-current="page">Worktrees</button>
      <button disabled>Providers</button>
      <button disabled>Skills</button>
      <button disabled>Agents <span>later</span></button>
      <button disabled>Proofs</button>
    </nav>
    <div class="worktree-settings-main">
      {#if view}
        <section>
          <h2>Worktree location</h2>
          <p class="worktree-description">
            Every thread and every agent works in its own worktree, so parallel
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
                aria-label="Worktree folder"
                value={view.folder}
                spellcheck="false"
                onkeydown={blurOnEscape}
                oninput={(event) => edit({ Folder: event.currentTarget.value })}
              /></label
            >
            <button class="worktree-button browse" onclick={() => void browse()}
              >Browse…</button
            >
          </div>
          <div class="worktree-path-preview">
            {view.preview_root}<span>&lt;project&gt;</span>{view.separator}<span
              >&lt;branch&gt;</span
            ><br /><small>e.g. {view.example_path}</small>
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
                  aria-label="Project worktree location"
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
            A worktree lives exactly as long as its work.
          </p>
          <div class="worktree-checks">
            <label class="worktree-check locked"
              ><input type="checkbox" checked disabled />Create a worktree
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
              its PR merges
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
              />Delete the local branch too</label
            >
          </div>
          <div class="worktree-setting-row lifecycle-first">
            <span>PR closed without merging</span><span class="hint"
              >worktree kept · right-click the thread → Delete removes both</span
            >
          </div>
          <div class="worktree-setting-row">
            <span>Thread failed</span><span class="hint"
              >worktree kept · same: delete the thread to remove it</span
            >
          </div>
          <div class="worktree-setting-row">
            <span>Spec interview</span><span class="hint"
              >no worktree · it only reads the repo and writes SPEC.md</span
            >
          </div>
        </section>
        <section>
          <h2>New worktree setup</h2>
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
            <label for="worktree-setup">Run after creating</label><label
              class="worktree-input"
              ><input
                id="worktree-setup"
                value={view.setup_command}
                spellcheck="false"
                onkeydown={blurOnEscape}
                oninput={(event) =>
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
              />Share the Rust build cache between a project's worktrees</label
            ><span class="hint"
              >{view.share_build_cache
                ? 'no cold builds · ≈2 GB less each'
                : 'each worktree builds from scratch'}</span
            >
          </div>
          <div class="worktree-setting-row">
            <label for="worktree-ready">Keep N worktrees ready</label><label
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
        <section>
          <h2>Live worktrees</h2>
          <p class="worktree-description">
            Every worktree Velocity created and hasn't deleted yet.
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
          {#if view.rows.length === 0}<p class="worktree-description">
              No live worktrees.
            </p>{/if}
          <p class="worktree-size-note">
            Sizes exclude linked directories and shared build caches outside the
            worktree.
          </p>
        </section>
      {:else}<p class="worktree-description">Reading worktrees…</p>{/if}
      {#if error}<p class="worktree-message error" role="alert">{error}</p>{/if}
      {#if notice}<p class="worktree-message" role="status">{notice}</p>{/if}
    </div>
  </div>
</section>
