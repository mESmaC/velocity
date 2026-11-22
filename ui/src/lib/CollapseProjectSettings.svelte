<script lang="ts">
  import PaneState from './PaneState.svelte';
  import {
    collapseCommands,
    releaseCommands,
    type CollapseFeedHealth,
    type CollapseRelayKind,
    type IssueHost,
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
    loadError = '';
    actionError = '';
    notice = '';
    void load(target);
    const timer = globalThis.setInterval(() => {
      if (!busy) void load(target);
    }, 5000);
    return () => globalThis.clearInterval(timer);
  });

  function formatError(reason: unknown): string {
    return reason instanceof Error ? reason.message : String(reason);
  }

  function formatTimestamp(ms: string | null | undefined): string {
    const value = ms ? Number(ms) : Number.NaN;
    return Number.isFinite(value)
      ? new Date(value).toLocaleString()
      : 'Not sent yet';
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

  async function linkProject(mode: 'mirror' | 'live') {
    if (busy) return;
    const target = currentProject;
    busy = true;
    actionError = '';
    notice = '';
    try {
      if (mode === 'mirror') {
        await collapseCommands.mirror(target);
      } else {
        await collapseCommands.sendLiveState(target);
      }
      await load(target);
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
            Defaults to Collapse for projects mirrored to Collapse. Dispatch,
            holds, dependencies, AIRC issue tools and Rosen land/close comments
            follow this host.
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
              {releaseCreds.collapse_release_token_stored
                ? 'Stored in Windows Credential Manager'
                : releaseCreds.collapse_connected
                  ? 'Optional · using connected Collapse account'
                  : 'Not set'}
            </span>
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
              placeholder="Collapse PAT with releases scope"
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
