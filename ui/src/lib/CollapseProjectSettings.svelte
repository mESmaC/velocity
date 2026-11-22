<script lang="ts">
  import CollapseDeletedRepoNotice from './CollapseDeletedRepoNotice.svelte';
  import PaneState from './PaneState.svelte';
  import {
    collapseCommands,
    releaseCommands,
    type CollapseCheckItemStatus,
    type CollapseFeedHealth,
    type CollapseRelayKind,
    type DeletedCollapseRepository,
    type DeletedRepositoryAction,
    type IssueHost,
    type MoveToCollapseResult,
    type MoveToCollapseStepStatus,
    type ProjectCollapseSettings,
    type PushTokenStatus,
    type ReleaseCredentialsStatus,
  } from './ipc';

  let { project }: { project: number } = $props();

  const relayOptions: ReadonlyArray<{
    kind: CollapseRelayKind;
    label: string;
  }> = [
    { kind: 'approve', label: 'Approve changes (approve)' },
    { kind: 'land', label: 'Land changes (land)' },
    { kind: 'mark', label: 'Mark landed changes reviewed (mark)' },
    { kind: 'config', label: 'Propose rosen.toml changes (config)' },
    { kind: 'revoke', label: 'Revoke enrolled machines (revoke)' },
  ];

  let settings = $state<ProjectCollapseSettings | null | undefined>(undefined);
  let releaseCreds = $state<ReleaseCredentialsStatus | null>(null);
  let signingKeyInput = $state('');
  let signingPasswordInput = $state('');
  let releaseTokenInput = $state('');
  let issuesTokenInput = $state('');
  let moveCollapseRepoInput = $state('');
  let moveGitHubRepoInput = $state('');
  let moveGitHubTokenInput = $state('');
  let moveKeepDownstream = $state(false);
  let moveDefaultsLoadedFor = $state<number | null>(null);
  let moving = $state(false);
  let moveResult = $state<MoveToCollapseResult | null>(null);
  let mirrorDeleted = $state<DeletedCollapseRepository | null>(null);
  let loadError = $state('');
  let actionError = $state('');
  let notice = $state('');
  let busy = $state(false);

  const currentProject = $derived(project);

  async function load(target: number) {
    try {
      const [loaded, creds] = await Promise.all([
        collapseCommands.projectSettings(target),
        releaseCommands.credentialsStatus().catch(() => null),
      ]);
      if (target === currentProject) {
        settings = loaded;
        releaseCreds = creds;
        if (loaded && moveDefaultsLoadedFor !== target) {
          moveDefaultsLoadedFor = target;
          moveCollapseRepoInput = loaded.collapse_repository ?? '';
          moveGitHubRepoInput =
            loaded.github_repository ?? loaded.downstream_github ?? '';
          moveKeepDownstream = Boolean(loaded.downstream_github);
        }
        loadError = '';
      }
    } catch (reason) {
      if (target === currentProject) {
        loadError = reason instanceof Error ? reason.message : String(reason);
      }
    }
  }

  $effect(() => {
    const target = currentProject;
    settings = undefined;
    moveDefaultsLoadedFor = null;
    moveCollapseRepoInput = '';
    moveGitHubRepoInput = '';
    moveGitHubTokenInput = '';
    moveKeepDownstream = false;
    moveResult = null;
    mirrorDeleted = null;
    moving = false;
    loadError = '';
    actionError = '';
    notice = '';
    void load(target);
    const timer = globalThis.setInterval(() => {
      if (!busy && !moving) void load(target);
    }, 5000);
    return () => globalThis.clearInterval(timer);
  });

  function stepStatusLabel(status: MoveToCollapseStepStatus): string {
    switch (status) {
      case 'passed':
        return 'Pass';
      case 'warning':
        return 'Attention';
      case 'skipped':
        return 'Skipped';
      case 'failed':
        return 'Failed';
    }
  }

  function checkStatusLabel(status: CollapseCheckItemStatus): string {
    switch (status) {
      case 'pass':
        return 'pass';
      case 'moved':
        return 'moved';
      case 'fail':
        return 'fail';
      case 'skipped':
        return 'skipped';
    }
  }

  function safeExternalUrl(url: string | null | undefined): string | null {
    if (!url) return null;
    try {
      const parsed = new globalThis.URL(url);
      return parsed.protocol === 'https:' ? parsed.toString() : null;
    } catch {
      return null;
    }
  }

  async function runMoveToCollapse(
    deletedAction: DeletedRepositoryAction | null = null,
  ) {
    if (busy || moving || !settings) return;
    const target = currentProject;
    moving = true;
    busy = true;
    actionError = '';
    notice = '';
    mirrorDeleted = null;
    try {
      const result = await collapseCommands.moveToCollapse({
        project_identifier: target,
        collapse_repository: moveCollapseRepoInput.trim() || null,
        github_repository: moveGitHubRepoInput.trim() || null,
        keep_github_downstream: moveKeepDownstream,
        github_import_token: moveGitHubTokenInput.trim() || null,
        deleted_action: deletedAction,
      });
      if (target === currentProject) {
        moveResult = result;
        settings = result.settings;
        moveGitHubTokenInput = '';
        if (result.collapse_repository) {
          moveCollapseRepoInput = result.collapse_repository;
        }
        if (result.github_repository && moveKeepDownstream) {
          moveGitHubRepoInput = result.github_repository;
        }
        if (result.deleted_repository) {
          notice = '';
        } else {
          notice = result.ok
            ? 'Move to Collapse completed.'
            : 'Move to Collapse finished with items that need attention.';
        }
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) {
        moving = false;
        busy = false;
      }
    }
  }

  function formatError(reason: unknown): string {
    return reason instanceof Error ? reason.message : String(reason);
  }

  function formatTimestamp(ms: string | null | undefined): string {
    const value = ms ? Number(ms) : Number.NaN;
    return Number.isFinite(value)
      ? new Date(value).toLocaleString()
      : 'Not sent yet';
  }

  function formatReleaseTokenDate(iso: string | null | undefined): string {
    if (!iso) return '';
    const parsed = new Date(iso);
    if (Number.isNaN(parsed.getTime())) return iso;
    return parsed.toLocaleDateString(undefined, { dateStyle: 'medium' });
  }

  function releaseTokenStatusLabel(creds: ReleaseCredentialsStatus): string {
    if (!creds.collapse_release_token_stored) {
      return creds.collapse_connected
        ? 'From sign-in · created automatically on next release'
        : 'Not set';
    }
    if (creds.collapse_release_token_source === 'sign_in') {
      const created = formatReleaseTokenDate(
        creds.collapse_release_token_created_at,
      );
      return created ? `From sign-in · created ${created}` : 'From sign-in';
    }
    return 'Pasted · stored in Windows Credential Manager';
  }

  function pushTokenLabel(status: PushTokenStatus): string {
    switch (status.kind) {
      case 'not_configured':
        return 'Not configured';
      case 'present':
        return 'Stored in git credential helper';
      case 'missing':
        return 'Missing from git credential helper';
      case 'partial':
        return status.state_feed
          ? 'Missing for ledger push'
          : 'Missing for state feed';
      case 'unavailable':
        return `Unavailable · ${status.error}`;
    }
  }

  function feedSummary(health: CollapseFeedHealth, linked: boolean): string {
    if (!linked && !health.last_sent_at && !health.last_error) return 'Off';
    return `Last sent ${formatTimestamp(health.last_sent_at)}`;
  }

  async function toggleChannel(enabled: boolean) {
    if (busy || !settings) return;
    const target = currentProject;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const result = await collapseCommands.setChannel(target, enabled);
      if (target === currentProject) {
        settings = result.settings;
        notice = result.message;
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) busy = false;
    }
  }

  async function toggleRelay(kind: CollapseRelayKind, checked: boolean) {
    if (busy || !settings) return;
    const target = currentProject;
    const next = checked
      ? [...new Set([...settings.relay_allow, kind])]
      : settings.relay_allow.filter((entry) => entry !== kind);
    busy = true;
    actionError = '';
    notice = '';
    try {
      const updated = await collapseCommands.setRelay(target, next);
      if (target === currentProject) {
        settings = updated;
        notice =
          updated.relay_allow.length > 0
            ? `Relay actions updated (${updated.relay_allow.join(', ')}).`
            : 'Collapse relay actions disabled.';
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) busy = false;
    }
  }

  async function linkProject(
    mode: 'mirror' | 'live',
    deletedAction: DeletedRepositoryAction | null = null,
  ) {
    if (busy) return;
    const target = currentProject;
    busy = true;
    actionError = '';
    notice = '';
    try {
      if (mode === 'mirror') {
        const result = await collapseCommands.mirror(target, deletedAction);
        if (target === currentProject) {
          mirrorDeleted = result.deleted_repository;
          if (!result.deleted_repository) {
            notice = result.message;
            await load(target);
          }
        }
      } else {
        mirrorDeleted = null;
        await collapseCommands.sendLiveState(target);
        await load(target);
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) busy = false;
    }
  }

  async function unlink() {
    if (busy || !settings?.collapse_root) return;
    if (
      !globalThis.confirm(
        'Unlink Collapse live state? This lands a rosen.toml change removing the collapse root and clears the relay allow-list.',
      )
    )
      return;
    const target = currentProject;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const result = await collapseCommands.unlink(target);
      if (target === currentProject) {
        settings = result.settings;
        notice = result.message;
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) busy = false;
    }
  }

  async function openOnCollapse() {
    actionError = '';
    try {
      await collapseCommands.open(currentProject);
    } catch (reason) {
      actionError = formatError(reason);
    }
  }

  async function saveSigningKey() {
    if (busy || !signingKeyInput.trim()) return;
    busy = true;
    actionError = '';
    notice = '';
    try {
      releaseCreds = await releaseCommands.setUpdaterSigningKey(
        signingKeyInput,
        signingPasswordInput.trim() ? signingPasswordInput : null,
      );
      signingKeyInput = '';
      signingPasswordInput = '';
      notice = 'Saved updater signing key in Windows Credential Manager.';
    } catch (reason) {
      actionError = formatError(reason);
    } finally {
      busy = false;
    }
  }

  async function clearSigningKey() {
    if (busy) return;
    busy = true;
    actionError = '';
    notice = '';
    try {
      releaseCreds = await releaseCommands.clearUpdaterSigningKey();
      notice = 'Removed updater signing key from Windows Credential Manager.';
    } catch (reason) {
      actionError = formatError(reason);
    } finally {
      busy = false;
    }
  }

  async function selectIssueHost(host: IssueHost) {
    if (busy || !settings || settings.issue_host === host) return;
    const target = currentProject;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const updated = await collapseCommands.setIssueHost(target, host);
      if (target === currentProject) {
        settings = updated;
        notice =
          host === 'collapse'
            ? 'Issues now run on Collapse (edge cursor + changes feed).'
            : 'Issues now run on GitHub.';
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) busy = false;
    }
  }

  async function toggleGithubIssueSync(enabled: boolean) {
    if (busy || !settings) return;
    const target = currentProject;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const updated = await collapseCommands.setGithubIssueSync(
        target,
        enabled,
      );
      if (target === currentProject) {
        settings = updated;
        notice = enabled
          ? 'Enabled one-way sync of newly opened GitHub issues into Collapse.'
          : 'Turned off one-way GitHub issue sync.';
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) busy = false;
    }
  }

  async function syncGithubIssuesNow() {
    if (busy || !settings) return;
    const target = currentProject;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const updated = await collapseCommands.syncGithubIssuesNow(target);
      if (target === currentProject) {
        settings = updated;
        notice = 'Checked GitHub for newly opened issues.';
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) busy = false;
    }
  }

  async function saveIssuesToken() {
    if (busy || !issuesTokenInput.trim()) return;
    const target = currentProject;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const updated = await collapseCommands.setIssuesToken(
        target,
        issuesTokenInput,
      );
      if (target === currentProject) {
        settings = updated;
        issuesTokenInput = '';
        notice = 'Saved Collapse issues token in Windows Credential Manager.';
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) busy = false;
    }
  }

  async function clearIssuesToken() {
    if (busy) return;
    const target = currentProject;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const updated = await collapseCommands.clearIssuesToken(target);
      if (target === currentProject) {
        settings = updated;
        notice =
          'Removed Collapse issues token from Windows Credential Manager.';
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) busy = false;
    }
  }

  async function saveReleaseToken() {
    if (busy || !releaseTokenInput.trim()) return;
    busy = true;
    actionError = '';
    notice = '';
    try {
      releaseCreds =
        await releaseCommands.setCollapseReleaseToken(releaseTokenInput);
      releaseTokenInput = '';
      notice = 'Saved Collapse release token in Windows Credential Manager.';
    } catch (reason) {
      actionError = formatError(reason);
    } finally {
      busy = false;
    }
  }

  async function replaceReleaseToken() {
    if (busy) return;
    busy = true;
    actionError = '';
    notice = '';
    try {
      releaseCreds = await releaseCommands.replaceCollapseReleaseToken();
      notice = 'Created a Collapse release token from the signed-in session.';
    } catch (reason) {
      actionError = formatError(reason);
    } finally {
      busy = false;
    }
  }

  async function clearReleaseToken() {
    if (busy) return;
    busy = true;
    actionError = '';
    notice = '';
    try {
      releaseCreds = await releaseCommands.clearCollapseReleaseToken();
      notice =
        'Removed Collapse release token from Windows Credential Manager.';
    } catch (reason) {
      actionError = formatError(reason);
    } finally {
      busy = false;
    }
  }
</script>

{#if settings !== null}
  <section aria-label="Collapse project settings">
    <h2>Collapse</h2>
    <p class="worktree-description">
      Live state feed, ledger push, channel lines and owner action relay for
      this project's Rosen server.
    </p>
    <PaneState
      name="Collapse settings"
      rosen
      loading={settings === undefined}
      error={loadError}
      retry={() => load(currentProject)}
      automatic={false}
    />
    {#if settings}
      {@const linked = settings.collapse_root !== null}
      {#if settings.config_error}
        <p class="worktree-message error" role="alert">
          {settings.config_error}
        </p>
      {/if}

      <div class="worktree-setting-row collapse-relay-block collapse-cutover">
        <div class="collapse-relay-header">
          <span>Move to Collapse</span>
          <span class="hint">
            Import GitHub issues, link and fast-forward an existing Collapse
            repository (or create one), set Issues on Collapse and CI from
            Rosen, configure GitHub downstream, and run
            <code>rosen collapse check</code>. Safe to re-run.
          </span>
        </div>
        <div class="collapse-cutover-fields">
          <label class="collapse-cutover-field">
            <span class="hint"
              >Collapse repository (blank auto-links existing)</span
            >
            <input
              type="text"
              aria-label="Collapse repository"
              placeholder={settings.collapse_repository ??
                settings.github_repository ??
                'owner/name or blank'}
              bind:value={moveCollapseRepoInput}
              disabled={busy || moving}
              spellcheck="false"
              autocomplete="off"
            />
          </label>
          <label class="collapse-cutover-field">
            <span class="hint"
              >GitHub repository (for issue import / downstream)</span
            >
            <input
              type="text"
              aria-label="GitHub repository for cut-over"
              placeholder={settings.github_repository ??
                settings.downstream_github ??
                'owner/repo (auto-detect)'}
              bind:value={moveGitHubRepoInput}
              disabled={busy || moving}
              spellcheck="false"
              autocomplete="off"
            />
          </label>
          <label class="collapse-cutover-field">
            <span class="hint"
              >GitHub token for issue import (optional; blank uses Collapse
              GitHub App)</span
            >
            <input
              type="password"
              aria-label="Optional GitHub token for issue import"
              placeholder="Optional GitHub token (not saved)"
              bind:value={moveGitHubTokenInput}
              disabled={busy || moving}
              autocomplete="off"
            />
          </label>
        </div>
        <div
          class="collapse-row-value collapse-cutover-radios"
          role="radiogroup"
          aria-label="GitHub downstream mirror"
        >
          <label class="worktree-check">
            <input
              type="radio"
              name={`move-downstream-${currentProject}`}
              checked={!moveKeepDownstream}
              disabled={busy || moving}
              onchange={() => (moveKeepDownstream = false)}
            />
            Remove GitHub downstream
          </label>
          <label class="worktree-check">
            <input
              type="radio"
              name={`move-downstream-${currentProject}`}
              checked={moveKeepDownstream}
              disabled={busy || moving}
              onchange={() => (moveKeepDownstream = true)}
            />
            Keep GitHub as a one-way downstream copy
          </label>
        </div>
        <div class="collapse-row-value">
          <button
            type="button"
            class="worktree-button"
            disabled={busy ||
              moving ||
              !settings.collapse_connected ||
              Boolean(settings.config_error)}
            onclick={() => void runMoveToCollapse()}
          >
            {moving
              ? 'Moving to Collapse…'
              : moveResult || (linked && settings.issue_host === 'collapse')
                ? 'Re-run Move to Collapse'
                : 'Move to Collapse'}
          </button>
          {#if !settings.collapse_connected}
            <span class="hint">
              Connect Collapse in Settings → Agents → Shared MCP servers first.
            </span>
          {:else if !settings.collapse_issues_token_stored}
            <span class="hint">
              Save a Collapse PAT with the issues scope below before importing
              issues.
            </span>
          {/if}
        </div>
        {#if moveResult}
          {#if moveResult.deleted_repository}
            <CollapseDeletedRepoNotice
              deleted={moveResult.deleted_repository}
              busy={busy || moving}
              onRestore={() => void runMoveToCollapse('restore')}
              onPurgeAndCreate={() =>
                void runMoveToCollapse('purge_and_create')}
            />
          {/if}
          <ol
            class="collapse-cutover-steps"
            aria-label="Move to Collapse steps"
          >
            {#each moveResult.steps as step (step.step)}
              {@const externalUrl = safeExternalUrl(step.action_url)}
              <li class="collapse-cutover-step">
                <div class="collapse-cutover-step-head">
                  <span
                    class="collapse-cutover-badge"
                    class:collapse-status-ok={step.status === 'passed'}
                    class:collapse-status-warn={step.status === 'warning'}
                    class:collapse-status-err={step.status === 'failed'}
                    class:hint={step.status === 'skipped'}
                  >
                    {stepStatusLabel(step.status)}
                  </span>
                  <strong>{step.title}</strong>
                </div>
                <p class="collapse-cutover-summary">{step.summary}</p>
                {#if step.detail}
                  <p class="hint collapse-cutover-detail">{step.detail}</p>
                {/if}
                {#if externalUrl}
                  <a
                    href={externalUrl}
                    target="_blank"
                    rel="noopener noreferrer"
                    class="worktree-button mini collapse-cutover-link"
                    onclick={(event) => {
                      event.preventDefault();
                      void collapseCommands.openPage(externalUrl);
                    }}
                  >
                    {step.action_label ?? 'Open on Collapse'} ↗
                  </a>
                {/if}
              </li>
            {/each}
          </ol>
          {#if moveResult.collapse_check}
            <div class="collapse-check-report">
              <div class="collapse-cutover-step-head">
                <span
                  class="collapse-cutover-badge"
                  class:collapse-status-ok={moveResult.collapse_check.ok}
                  class:collapse-status-err={!moveResult.collapse_check.ok}
                >
                  {moveResult.collapse_check.ok ? 'OK' : 'Failed'}
                </span>
                <code>rosen collapse check</code>
              </div>
              <ul class="collapse-check-list">
                {#each moveResult.collapse_check.checks as item (item.name)}
                  <li class="collapse-check-item">
                    <span
                      class="collapse-check-status"
                      class:collapse-status-ok={item.status === 'pass'}
                      class:collapse-status-warn={item.status === 'moved'}
                      class:collapse-status-err={item.status === 'fail'}
                      class:hint={item.status === 'skipped'}
                    >
                      {checkStatusLabel(item.status)}
                    </span>
                    <code class="collapse-check-name">{item.name}</code>
                    <span class="collapse-check-detail">{item.detail}</span>
                    {#if item.hint}
                      <span class="hint collapse-check-hint">{item.hint}</span>
                    {/if}
                  </li>
                {/each}
              </ul>
            </div>
          {/if}
        {/if}
      </div>

      <div class="worktree-setting-row">
        <span>Collapse root</span>
        <div class="collapse-row-value">
          {#if settings.collapse_root}
            <code class="collapse-root">{settings.collapse_root}</code>
          {:else if settings.collapse_repository}
            <span class="hint">Not linked · commits only</span>
          {:else}
            <span class="hint">Not linked</span>
          {/if}
          {#if settings.collapse_repository}
            <button
              type="button"
              class="worktree-button mini"
              disabled={busy}
              onclick={() => void openOnCollapse()}>Open on Collapse ↗</button
            >
          {/if}
          {#if !linked}
            <button
              type="button"
              class="worktree-button mini"
              disabled={busy || Boolean(settings.config_error)}
              onclick={() =>
                void linkProject(
                  settings?.collapse_repository ? 'live' : 'mirror',
                )}
            >
              {busy
                ? 'Linking…'
                : settings.collapse_repository
                  ? 'Send Collapse its live state'
                  : 'Mirror to Collapse'}
            </button>
          {/if}
        </div>
        {#if mirrorDeleted}
          <CollapseDeletedRepoNotice
            deleted={mirrorDeleted}
            {busy}
            onRestore={() => void linkProject('mirror', 'restore')}
            onPurgeAndCreate={() =>
              void linkProject('mirror', 'purge_and_create')}
          />
        {/if}
      </div>

      <div class="worktree-setting-row">
        <span>Push token</span>
        <div class="collapse-row-value">
          <span
            class:collapse-status-ok={settings.push_token.kind === 'present'}
            class:collapse-status-warn={settings.push_token.kind === 'partial'}
            class:collapse-status-err={settings.push_token.kind === 'missing' ||
              settings.push_token.kind === 'unavailable'}
            class:hint={settings.push_token.kind === 'not_configured'}
          >
            {pushTokenLabel(settings.push_token)}
          </span>
          {#if settings.collapse_repository && (settings.push_token.kind === 'missing' || settings.push_token.kind === 'partial')}
            <button
              type="button"
              class="worktree-button mini"
              disabled={busy}
              onclick={() => void linkProject('live')}
              >{busy ? 'Updating…' : 'Restore push token'}</button
            >
          {/if}
        </div>
      </div>

      <div class="worktree-setting-row">
        <span>State feed</span>
        <div class="collapse-health">
          <span class="hint">{feedSummary(settings.state_feed, linked)}</span>
          {#if settings.state_feed.last_error}
            <p class="worktree-message error collapse-error" role="alert">
              {settings.state_feed.last_error}
            </p>
          {/if}
        </div>
      </div>

      <div class="worktree-setting-row">
        <span>Ledger push</span>
        <div class="collapse-health">
          <span class="hint">{feedSummary(settings.ledger_push, linked)}</span>
          {#if settings.ledger_push.last_error}
            <p class="worktree-message error collapse-error" role="alert">
              {settings.ledger_push.last_error}
            </p>
          {/if}
        </div>
      </div>

      <div class="worktree-setting-row collapse-relay-block">
        <div class="collapse-relay-header">
          <span>Issues on</span>
          <span class="hint">
            Stays on GitHub until switched explicitly or after Move to Collapse
            imports issues. Dispatch, holds, dependencies, AIRC issue tools and
            Rosen land/close comments follow this host.
          </span>
        </div>
        <div
          class="collapse-row-value"
          role="radiogroup"
          aria-label="Issues host"
        >
          <label class="worktree-check">
            <input
              type="radio"
              name={`issue-host-${currentProject}`}
              value="github"
              checked={settings.issue_host === 'github'}
              disabled={busy}
              onchange={() => void selectIssueHost('github')}
            />
            GitHub
          </label>
          <label class="worktree-check">
            <input
              type="radio"
              name={`issue-host-${currentProject}`}
              value="collapse"
              checked={settings.issue_host === 'collapse'}
              disabled={busy || !settings.collapse_repository}
              onchange={() => void selectIssueHost('collapse')}
            />
            Collapse
          </label>
          {#if !settings.collapse_repository}
            <span class="hint">Mirror to Collapse first</span>
          {/if}
        </div>
        <div class="collapse-row-value">
          <span>Collapse issues token</span>
          <span
            class:collapse-status-ok={settings.collapse_issues_token_stored}
            class:hint={!settings.collapse_issues_token_stored}
          >
            {settings.collapse_issues_token_stored
              ? 'Stored in Windows Credential Manager'
              : settings.collapse_connected
                ? 'Read-only · save a PAT with issues scope to write'
                : 'Not set'}
          </span>
          {#if settings.collapse_issues_token_stored}
            <button
              type="button"
              class="worktree-button mini"
              disabled={busy}
              onclick={() => void clearIssuesToken()}>Clear</button
            >
          {/if}
        </div>
        <form
          class="collapse-row-value"
          onsubmit={(event) => {
            event.preventDefault();
            void saveIssuesToken();
          }}
        >
          <input
            type="password"
            aria-label="Collapse issues token"
            placeholder="Collapse PAT with issues scope"
            bind:value={issuesTokenInput}
            disabled={busy}
            autocomplete="off"
          />
          <button
            type="submit"
            class="worktree-button mini"
            disabled={busy || !issuesTokenInput.trim()}>Save token</button
          >
        </form>
        {#if settings.issue_host === 'collapse'}
          {@const issueSync = settings.github_issue_sync}
          <div class="collapse-row-value">
            <label class="worktree-check">
              <input
                type="checkbox"
                checked={issueSync?.enabled ?? false}
                disabled={busy}
                onchange={(event) =>
                  void toggleGithubIssueSync(event.currentTarget.checked)}
              />
              One-way sync of new GitHub issues to Collapse
            </label>
            {#if issueSync?.enabled}
              <button
                type="button"
                class="worktree-button mini"
                disabled={busy}
                onclick={() => void syncGithubIssuesNow()}>Sync now</button
              >
            {/if}
          </div>
          {#if issueSync}
            <span class="hint">
              {#if issueSync.enabled}
                Watching {issueSync.github_repository}{#if issueSync.import_finished_at}
                  since {new Date(
                    issueSync.import_finished_at,
                  ).toLocaleString()}{:else}
                  above #{issueSync.high_water_number}{/if}
                · {issueSync.synced_count} synced{#if issueSync.last_synced_at_ms}
                  · last checked {formatTimestamp(
                    issueSync.last_synced_at_ms,
                  )}{/if}
              {:else if issueSync.github_repository}
                Off ({issueSync.github_repository}, {issueSync.synced_count} synced)
              {:else}
                Off
              {/if}
            </span>
            {#if issueSync.last_error}
              <p class="worktree-message error collapse-error" role="alert">
                {issueSync.last_error}
              </p>
            {/if}
          {:else}
            <span class="hint">
              Mirrors every GitHub issue opened or changed since Collapse's
              import finished until turned off.
            </span>
          {/if}
        {/if}
      </div>

      <div class="worktree-setting-row">
        <label class="worktree-check">
          <input
            type="checkbox"
            checked={settings.collapse_channel}
            disabled={busy || !linked || Boolean(settings.config_error)}
            onchange={(event) =>
              void toggleChannel(event.currentTarget.checked)}
          />
          Send channel lines to Collapse (<code>collapse_channel</code>)
        </label>
        <span class="hint">
          Lands a <code>rosen.toml</code> change to populate Collapse's Channel tab.
        </span>
      </div>

      <div class="worktree-setting-row collapse-relay-block">
        <div class="collapse-relay-header">
          <span>Relay actions</span>
          <span class="hint">
            Actions Collapse's owner pages may run on this server, re-applied on
            every start.
          </span>
        </div>
        <div class="worktree-checks collapse-relay-list">
          {#each relayOptions as option (option.kind)}
            <label class="worktree-check">
              <input
                type="checkbox"
                checked={settings.relay_allow.includes(option.kind)}
                disabled={busy || !linked}
                onchange={(event) =>
                  void toggleRelay(option.kind, event.currentTarget.checked)}
              />
              {option.label}
            </label>
          {/each}
        </div>
        {#if settings.relay_revoked}
          <p class="worktree-message error collapse-error" role="alert">
            The Collapse relay machine is revoked on this server. Enabling any
            action re-enrolls it.
          </p>
        {/if}
      </div>

      {#if linked}
        <div class="worktree-setting-row">
          <div>
            <span>Unlink Collapse</span>
            <p class="worktree-description collapse-unlink-hint">
              Removes <code>collapse</code> from <code>rosen.toml</code> through a
              landed change and clears the relay allow-list.
            </p>
          </div>
          <button
            type="button"
            class="worktree-button mini collapse-danger"
            disabled={busy || Boolean(settings.config_error)}
            onclick={() => void unlink()}
          >
            {busy ? 'Unlinking…' : 'Unlink'}
          </button>
        </div>
      {/if}

      {#if releaseCreds}
        <div class="worktree-setting-row collapse-relay-block">
          <div class="collapse-relay-header">
            <span>Release credentials</span>
            <span class="hint">
              Stored in Windows Credential Manager for local signed releases to
              Collapse; never shown again or passed to agents.
            </span>
          </div>
          <div class="collapse-row-value">
            <span>Updater signing key</span>
            <span
              class:collapse-status-ok={releaseCreds.updater_key_stored}
              class:hint={!releaseCreds.updater_key_stored}
            >
              {releaseCreds.updater_key_stored
                ? 'Stored in Windows Credential Manager'
                : 'Not set'}
            </span>
            {#if releaseCreds.updater_key_stored}
              <button
                type="button"
                class="worktree-button mini"
                disabled={busy}
                onclick={() => void clearSigningKey()}>Clear</button
              >
            {/if}
          </div>
          <form
            class="collapse-row-value"
            onsubmit={(event) => {
              event.preventDefault();
              void saveSigningKey();
            }}
          >
            <input
              type="password"
              aria-label="Updater signing private key"
              placeholder="Updater signing private key"
              bind:value={signingKeyInput}
              disabled={busy}
              autocomplete="off"
            />
            <input
              type="password"
              aria-label="Updater signing key password"
              placeholder="Key password (optional)"
              bind:value={signingPasswordInput}
              disabled={busy}
              autocomplete="off"
            />
            <button
              type="submit"
              class="worktree-button mini"
              disabled={busy || !signingKeyInput.trim()}>Save key</button
            >
          </form>
          <div class="collapse-row-value">
            <span>Collapse release token</span>
            <span
              class:collapse-status-ok={releaseCreds.collapse_release_token_stored}
              class:hint={!releaseCreds.collapse_release_token_stored}
            >
              {releaseTokenStatusLabel(releaseCreds)}
            </span>
            {#if releaseCreds.collapse_release_token_source === 'sign_in'}
              <button
                type="button"
                class="worktree-button mini"
                disabled={busy || !releaseCreds.collapse_connected}
                onclick={() => void replaceReleaseToken()}>Replace</button
              >
            {:else if !releaseCreds.collapse_release_token_stored && releaseCreds.collapse_connected}
              <button
                type="button"
                class="worktree-button mini"
                disabled={busy}
                onclick={() => void replaceReleaseToken()}>Create</button
              >
            {/if}
            {#if releaseCreds.collapse_release_token_stored}
              <button
                type="button"
                class="worktree-button mini"
                disabled={busy}
                onclick={() => void clearReleaseToken()}>Clear</button
              >
            {/if}
          </div>
          <form
            class="collapse-row-value"
            onsubmit={(event) => {
              event.preventDefault();
              void saveReleaseToken();
            }}
          >
            <input
              type="password"
              aria-label="Collapse release token"
              placeholder="Paste Collapse PAT with releases scope (overrides sign-in)"
              bind:value={releaseTokenInput}
              disabled={busy}
              autocomplete="off"
            />
            <button
              type="submit"
              class="worktree-button mini"
              disabled={busy || !releaseTokenInput.trim()}>Save token</button
            >
          </form>
        </div>
      {/if}

      {#if actionError}
        <p class="worktree-message error" role="alert">{actionError}</p>
      {:else if notice}
        <p class="worktree-message" role="status">{notice}</p>
      {/if}
    {/if}
  </section>
{/if}
