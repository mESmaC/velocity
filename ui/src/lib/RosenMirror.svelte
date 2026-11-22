<script lang="ts">
  import {
    collapseCommands,
    projectHealthCommands,
    rosenMirrorCommands,
  } from './ipc';
  import type {
    BlockedDispatchIssue,
    CollapseMirrorHealth,
    DispatchIdleIssueAction,
    LayoutAction,
    MirrorBehind,
    MirrorDivergence,
    MirrorState,
    ProjectHealthItem,
  } from './ipc';

  // `state` is renamed here: the name would shadow the `$state` rune.
  let {
    state: mirror,
    behind = null,
    divergence = null,
    project = null,
    collapse = null,
    collapseForwarding = null,
    collapseIssuesImport = null,
    hostSourceChange = null,
    dispatchIdle = null,
    send,
  }: {
    state: MirrorState | null;
    // The project's base trails its mirror while a catch-up is pending (#520).
    behind?: MirrorBehind | null;
    // The base and mirror diverged at the last catch-up (#501); Rust keeps it.
    // Only the project header shows it; Settings shows push failures alone.
    divergence?: MirrorDivergence | null;
    project?: number | null;
    // Collapse's forwarder to the downstream remote (#427). Only a stopped
    // forwarder or an archived repository needs the owner, so only those show.
    collapse?: CollapseMirrorHealth | null;
    // Missing or failing GitHub downstream copy surfaced in the title bar (#611).
    collapseForwarding?: ProjectHealthItem | null;
    // Collapse issue host whose GitHub issues were never imported (#612).
    collapseIssuesImport?: ProjectHealthItem | null;
    // Non-owner change to effective issue host or base CI source (#613).
    hostSourceChange?: ProjectHealthItem | null;
    // Free agent slots idle for 30+ min with every open issue blocked (#616).
    dispatchIdle?: ProjectHealthItem | null;
    send?: (action: LayoutAction) => void;
  } = $props();

  let running = $state('');
  let error = $state('');
  let importingIssues = $state(false);
  let importError = $state('');
  let importActionUrl = $state<string | null>(null);
  let importActionLabel = $state<string | null>(null);
  let resolvingHostChange = $state(false);
  let hostChangeError = $state('');
  let idleActionBusy = $state<Record<number, DispatchIdleIssueAction>>({});
  let idleActionError = $state<Record<number, string>>({});

  $effect(() => {
    void project;
    void collapseIssuesImport;
    void hostSourceChange;
    void dispatchIdle;
    if (!collapseIssuesImport) {
      importError = '';
      importActionUrl = null;
      importActionLabel = null;
    }
    if (!hostSourceChange) {
      hostChangeError = '';
    }
    if (!dispatchIdle) {
      idleActionBusy = {};
      idleActionError = {};
    }
  });

  async function runIdleIssueAction(
    issue: BlockedDispatchIssue,
    action: DispatchIdleIssueAction,
  ) {
    const target = project;
    if (target === null || idleActionBusy[issue.number]) return;
    idleActionBusy = { ...idleActionBusy, [issue.number]: action };
    const nextErrors = { ...idleActionError };
    delete nextErrors[issue.number];
    idleActionError = nextErrors;
    try {
      await projectHealthCommands.actOnDispatchIdleIssue(
        target,
        issue.repository,
        issue.number,
        action,
      );
    } catch (reason) {
      if (target === project) {
        idleActionError = {
          ...idleActionError,
          [issue.number]:
            reason instanceof Error ? reason.message : String(reason),
        };
      }
    } finally {
      if (target === project) {
        const nextBusy = { ...idleActionBusy };
        delete nextBusy[issue.number];
        idleActionBusy = nextBusy;
      }
    }
  }

  async function resolveHostSourceChange(switchBack: boolean) {
    const target = project;
    if (target === null || resolvingHostChange) return;
    resolvingHostChange = true;
    hostChangeError = '';
    try {
      await projectHealthCommands.resolveHostChange(target, switchBack);
    } catch (reason) {
      if (target === project) {
        hostChangeError =
          reason instanceof Error ? reason.message : String(reason);
      }
    } finally {
      if (target === project) resolvingHostChange = false;
    }
  }

  async function importCollapseIssuesNow() {
    const target = project;
    if (target === null || importingIssues) return;
    importingIssues = true;
    importError = '';
    importActionUrl = null;
    importActionLabel = null;
    try {
      const step = await collapseCommands.importIssues(target);
      if (target === project && step.status !== 'passed') {
        importError = step.detail
          ? `${step.summary} ${step.detail}`
          : step.summary;
        importActionUrl = safeExternalUrl(step.action_url);
        importActionLabel = step.action_label ?? null;
      }
    } catch (reason) {
      if (target === project) {
        importError = reason instanceof Error ? reason.message : String(reason);
      }
    } finally {
      if (target === project) importingIssues = false;
    }
  }

  // Rosen answers with a toast; a refusal stays here beside the divergence.
  async function run(
    label: string,
    action: (project: number) => Promise<null>,
  ) {
    if (project === null || running) return;
    running = label;
    error = '';
    try {
      await action(project);
    } catch (reason) {
      error = reason instanceof Error ? reason.message : String(reason);
    } finally {
      running = '';
    }
  }

  function short(commit: string) {
    return commit.slice(0, 12);
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
</script>

{#if behind && !divergence}
  <section
    class="mirror-status"
    role="status"
    aria-label="Rosen mirror catch-up"
  >
    <strong
      >Behind the mirror by {behind.commits}
      {behind.commits === 1 ? 'commit' : 'commits'} · {behind.catching_up
        ? 'catching up…'
        : 'catch-up pending'}</strong
    >
    <span
      >Rosen base <code title={behind.base_commit}
        >{short(behind.base_commit)}</code
      >
      · mirror tip
      <code title={behind.mirror_commit}>{short(behind.mirror_commit)}</code
      ></span
    >
    {#if behind.error}<pre>{behind.error}</pre>{/if}
    {#if project !== null && !behind.catching_up}
      <div class="mirror-actions">
        <button
          class="mini"
          disabled={running !== ''}
          onclick={() => void run('Catching up…', rosenMirrorCommands.catchUp)}
          >Catch up now</button
        >
        {#if running}<span role="status">{running}</span>{/if}
      </div>
    {/if}
    {#if error}<p role="alert">{error}</p>{/if}
  </section>
{/if}
{#if divergence}
  <section
    class="mirror-status rejected"
    role="status"
    aria-label="Rosen mirror divergence"
  >
    <strong>Rosen base and mirror have diverged · needs a person</strong>
    <span
      >Rosen base <code title={divergence.base_commit}
        >{short(divergence.base_commit)}</code
      >
      · mirror tip
      <code title={divergence.mirror_commit}
        >{short(divergence.mirror_commit)}</code
      ></span
    >
    <pre>{divergence.detail}</pre>
    {#if project !== null}
      <div class="mirror-actions">
        <button
          class="mini"
          disabled={running !== ''}
          title="For a mirror whose edits Rosen already holds; Rosen refuses otherwise"
          onclick={() => void run('Adopting…', rosenMirrorCommands.adopt)}
          >Adopt mirror tip</button
        >
        <button
          class="mini"
          disabled={running !== ''}
          onclick={() => void run('Catching up…', rosenMirrorCommands.catchUp)}
          >Catch up again</button
        >
        {#if running}<span role="status">{running}</span>{/if}
      </div>
    {/if}
    {#if error}<p role="alert">{error}</p>{/if}
  </section>
{/if}
{#if dispatchIdle && dispatchIdle.dispatch_idle}
  {@const blockedIssues = dispatchIdle.dispatch_idle.blocked_issues ?? []}
  <section class="mirror-status" role="status" aria-label="Idle agent slots">
    <strong>{dispatchIdle.signal}</strong>
    <span>{dispatchIdle.action}</span>
    {#if blockedIssues.length > 0 && project !== null}
      <ul class="idle-issues">
        {#each blockedIssues as issue (issue.number)}
          {@const issueUrl = safeExternalUrl(issue.issue_url)}
          {@const busy = idleActionBusy[issue.number]}
          <li class="idle-issue-row">
            <div class="idle-issue-copy">
              <span class="idle-issue-headline" title={issue.title}>
                <b>#{issue.number}</b>
                {issue.reason}
              </span>
              {#if issue.last_attempt_result}
                <pre
                  class="idle-issue-result">Last attempt: {issue.last_attempt_result}</pre>
              {/if}
              {#if idleActionError[issue.number]}
                <p role="alert">{idleActionError[issue.number]}</p>
              {/if}
            </div>
            <div class="idle-issue-actions">
              {#if issue.can_lift_hold}
                <button
                  type="button"
                  class="mini"
                  disabled={Boolean(busy)}
                  onclick={() => void runIdleIssueAction(issue, 'lift_hold')}
                  >{busy === 'lift_hold' ? 'Lifting…' : 'Lift hold'}</button
                >
              {/if}
              {#if issue.can_retry_attempt}
                <button
                  type="button"
                  class="mini"
                  disabled={Boolean(busy)}
                  onclick={() =>
                    void runIdleIssueAction(issue, 'retry_attempt')}
                  >{busy === 'retry_attempt'
                    ? 'Clearing…'
                    : 'Try again'}</button
                >
              {/if}
              {#if issue.can_mark_needs_owner}
                <button
                  type="button"
                  class="mini"
                  disabled={Boolean(busy)}
                  onclick={() =>
                    void runIdleIssueAction(issue, 'mark_needs_owner')}
                  >{busy === 'mark_needs_owner'
                    ? 'Labelling…'
                    : 'Needs owner'}</button
                >
              {/if}
              {#if issue.last_attempt_thread && send}
                {@const attemptThread = issue.last_attempt_thread}
                <button
                  type="button"
                  class="mini"
                  onclick={() => send({ OpenThread: attemptThread })}
                  >Open thread</button
                >
              {/if}
              {#if issueUrl}
                <a
                  class="mini"
                  href={issueUrl}
                  target="_blank"
                  rel="noopener noreferrer"
                  onclick={(event) => {
                    event.preventDefault();
                    void collapseCommands.openPage(issueUrl);
                  }}>Open #{issue.number} ↗</a
                >
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    {:else if dispatchIdle.detail}
      <pre>{dispatchIdle.detail}</pre>
    {/if}
  </section>
{/if}
{#if hostSourceChange}
  {@const switchBack = hostSourceChange.host_switch_back}
  <section
    class="mirror-status"
    role="status"
    aria-label="Issue host or base CI change"
  >
    <strong>{hostSourceChange.signal}</strong>
    <span>{hostSourceChange.action}</span>
    {#if hostSourceChange.detail}<pre>{hostSourceChange.detail}</pre>{/if}
    {#if project !== null && switchBack}
      <div class="mirror-actions">
        {#if switchBack.can_switch_back}
          <button
            type="button"
            class="mini"
            disabled={resolvingHostChange}
            onclick={() => void resolveHostSourceChange(true)}
            >{resolvingHostChange
              ? 'Switching…'
              : switchBack.button_label}</button
          >
        {/if}
        <button
          type="button"
          class="mini"
          disabled={resolvingHostChange}
          onclick={() => void resolveHostSourceChange(false)}
          >{switchBack.keep_label}</button
        >
      </div>
    {/if}
    {#if hostChangeError}<p role="alert">{hostChangeError}</p>{/if}
  </section>
{/if}
{#if collapseIssuesImport}
  <section
    class="mirror-status"
    role="status"
    aria-label="Collapse issue import status"
  >
    <strong>{collapseIssuesImport.signal}</strong>
    {#if collapseIssuesImport.detail}<pre>{collapseIssuesImport.detail}</pre>{/if}
    {#if project !== null}
      <div class="mirror-actions">
        <button
          type="button"
          class="mini"
          disabled={importingIssues}
          onclick={() => void importCollapseIssuesNow()}
          >{importingIssues ? 'Importing…' : 'Import now'}</button
        >
        {#if importActionUrl}
          <a
            class="mini"
            href={importActionUrl}
            target="_blank"
            rel="noopener noreferrer"
            onclick={(event) => {
              event.preventDefault();
              if (importActionUrl)
                void collapseCommands.openPage(importActionUrl);
            }}>{importActionLabel ?? 'Open'} ↗</a
          >
        {/if}
      </div>
    {/if}
    {#if importError}<p role="alert">{importError}</p>{/if}
  </section>
{/if}
{#if collapseForwarding && !collapse?.archived}
  {@const settingsUrl = safeExternalUrl(collapseForwarding.action_url)}
  <section
    class="mirror-status rejected"
    role="status"
    aria-label="GitHub downstream copy status"
  >
    <strong>{collapseForwarding.signal}</strong>
    {#if collapseForwarding.detail}<pre>{collapseForwarding.detail}</pre>{/if}
    {#if settingsUrl}
      <div class="mirror-actions">
        <a
          class="mini"
          href={settingsUrl}
          target="_blank"
          rel="noopener noreferrer"
          title="Configure the GitHub downstream in Collapse repository settings"
          onclick={(event) => {
            event.preventDefault();
            void collapseCommands.openPage(settingsUrl);
          }}>{collapseForwarding.action_label ?? 'Open Collapse settings'} ↗</a
        >
      </div>
    {/if}
  </section>
{/if}
{#if collapse && (collapse.archived || (collapse.forwarder === 'Stopped' && !collapseForwarding))}
  <section
    class="mirror-status rejected"
    role="status"
    aria-label="Collapse forwarder status"
  >
    <strong
      >{collapse.archived
        ? 'Collapse repository archived · lands are refused'
        : 'Collapse forwarder stopped · needs a person'}</strong
    >
    {#if collapse.message}<pre>{collapse.message}</pre>{/if}
    {#if project !== null}
      <div class="mirror-actions">
        <button
          class="mini"
          title="Retry or change the downstream remote on Collapse"
          onclick={() =>
            void run('Opening…', (identifier) =>
              collapseCommands.open(identifier).then(() => null),
            )}>Open on Collapse ↗</button
        >
      </div>
    {/if}
    {#if error}<p role="alert">{error}</p>{/if}
  </section>
{/if}
{#if mirror}
  <section
    class="mirror-status"
    class:rejected={mirror.rejected}
    role="status"
    aria-label="Rosen mirror status"
  >
    <strong
      >Rosen mirror {mirror.rejected
        ? 'rejected · needs a person'
        : 'failing · retrying'}</strong
    >
    {#if mirror.retry_at_ms}
      <span
        >Next retry: {new Date(
          Number(mirror.retry_at_ms),
        ).toLocaleString()}</span
      >
    {/if}
    <pre>{mirror.error}</pre>
    {#if mirror.base_commit}
      <span
        >Rosen base <code title={mirror.base_commit}
          >{short(mirror.base_commit)}</code
        >
        · last mirrored commit
        <code title={mirror.last_pushed_commit ?? undefined}
          >{mirror.last_pushed_commit
            ? short(mirror.last_pushed_commit)
            : 'none'}</code
        ></span
      >
    {:else}
      <span
        >Last mirrored commit: <code
          >{mirror.last_pushed_commit ?? 'no accepted commit recorded'}</code
        ></span
      >
    {/if}
  </section>
{/if}

<style>
  .mirror-status {
    padding: var(--worktrees-drive-padding);
    border-bottom: var(--worktrees-head-border);
    background: var(--surface);
    color: var(--warn);
    font-size: var(--reference-body-font-size);
    line-height: var(--reference-body-line-height);
    overflow-wrap: anywhere;
  }
  .mirror-status.rejected {
    color: var(--err);
  }
  strong,
  span {
    display: block;
  }
  pre {
    margin: var(--worktrees-zero);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--text-2);
  }
  pre,
  code {
    font-family: var(--mono);
  }
  .mirror-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-3);
    margin-top: var(--sp-3);
  }
  .mirror-actions span {
    color: var(--text-2);
  }
  p {
    margin: var(--worktrees-zero);
  }
  .idle-issues {
    display: grid;
    gap: var(--sp-2);
    max-height: 168px;
    margin: var(--sp-3) 0 0;
    padding: 0;
    overflow-y: auto;
    list-style: none;
  }
  .idle-issue-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2) var(--sp-4);
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--line-dim);
    border-radius: var(--r-sm);
    background: var(--card);
  }
  .idle-issue-copy {
    display: grid;
    gap: var(--sp-1);
    min-width: 0;
    flex: 1;
  }
  .idle-issue-headline {
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
  }
  .idle-issue-headline b {
    color: var(--text);
    font-weight: var(--fw-semibold);
  }
  .idle-issue-result {
    font-size: var(--fs-xs);
    color: var(--muted);
  }
  .idle-issue-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
</style>
