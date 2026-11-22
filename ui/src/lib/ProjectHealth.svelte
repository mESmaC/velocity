<script lang="ts">
  import { SvelteMap } from 'svelte/reactivity';
  import Icon from './Icon.svelte';
  import {
    collapseCommands,
    onProjectHealth,
    projectHealthCommands,
    type BlockedDispatchIssue,
    type DispatchIdleIssueAction,
    type HealthSeverity,
    type LayoutAction,
    type ProjectHealthHistoryEntry,
    type ProjectHealthItem,
  } from './ipc';

  // Project health notifications (#509, #616). A bell in the title bar counts the active
  // signals and lists them in a dropdown alongside persisted health history. A signal that
  // appears or changes while the project is open also pops out under the bell for a few
  // seconds. Signals already active when a project loads only set the badge, so switching
  // projects doesn't replay old pop-outs.
  let {
    project = null,
    send,
    onForwardingWarning,
    onIssuesImportWarning,
    onHostSourceChangeWarning,
    onDispatchIdleWarning,
  }: {
    project: number | null;
    send: (action: LayoutAction) => void;
    onForwardingWarning?: (item: ProjectHealthItem | null) => void;
    onIssuesImportWarning?: (item: ProjectHealthItem | null) => void;
    onHostSourceChangeWarning?: (item: ProjectHealthItem | null) => void;
    onDispatchIdleWarning?: (item: ProjectHealthItem | null) => void;
  } = $props();

  const POPOUT_MS = 8000;
  const MAX_POPOUTS = 3;
  const RANK: Record<HealthSeverity, number> = {
    Info: 0,
    Warning: 1,
    Error: 2,
  };

  let items = $state.raw<ProjectHealthItem[]>([]);
  let history = $state.raw<ProjectHealthHistoryEntry[]>([]);
  let popouts = $state.raw<ProjectHealthItem[]>([]);
  let menu = $state<HTMLElement>();
  let importingIssues = $state(false);
  let importIssuesError = $state('');
  let importIssuesUrl = $state<string | null>(null);
  let importIssuesUrlLabel = $state<string | null>(null);
  let resolvingHostChange = $state(false);
  let hostChangeError = $state('');
  let copyingIssueNumber = $state<number | null>(null);
  let dismissingIssueNumber = $state<number | null>(null);
  let hostIssueError = $state<Record<number, string>>({});
  let idleActionBusy = $state<Record<number, DispatchIdleIssueAction>>({});
  let idleActionError = $state<Record<number, string>>({});
  // What each signal said when last shown. The timestamp is left out, so a check
  // that re-confirms the same signal doesn't pop it out again.
  let seen = new Map<string, string>();
  const timers = new SvelteMap<string, ReturnType<typeof setTimeout>>();
  const currentProject = $derived(project);
  const worst = $derived(
    items.reduce<HealthSeverity | null>(
      (top, item) =>
        top === null || RANK[item.severity] > RANK[top] ? item.severity : top,
      null,
    ),
  );

  // DispatchIdle pops out strictly once per idle spell (keyed by idle_since_ms) rather
  // than re-popping every tick as its duration advances (#616).
  const fingerprint = (item: ProjectHealthItem) =>
    item.kind === 'DispatchIdle'
      ? `DispatchIdle|${item.dispatch_idle?.idle_since_ms ?? item.acted_at_ms}`
      : `${item.severity}|${item.signal}|${item.action}|${item.action_url ?? ''}|${item.host_switch_back?.button_label ?? ''}|${item.host_switch_back?.can_switch_back ?? ''}|${item.host_issue_action?.issue_number ?? ''}|${item.host_issue_action?.can_copy_from_github ?? ''}`;

  function safeExternalUrl(url: string | null | undefined): string | null {
    if (!url) return null;
    try {
      const parsed = new globalThis.URL(url);
      return parsed.protocol === 'https:' ? parsed.toString() : null;
    } catch {
      return null;
    }
  }

  function publishHeaderWarnings(next: ProjectHealthItem[]) {
    onForwardingWarning?.(
      next.find((item) => item.kind === 'CollapseForwarding') ?? null,
    );
    const importItem =
      next.find((item) => item.kind === 'CollapseIssuesNotImported') ?? null;
    onIssuesImportWarning?.(importItem);
    if (!importItem) {
      importIssuesError = '';
      importIssuesUrl = null;
      importIssuesUrlLabel = null;
    }
    const hostChangeItem =
      next.find((item) => item.kind === 'HostSourceChanged') ?? null;
    onHostSourceChangeWarning?.(hostChangeItem);
    if (!hostChangeItem) {
      hostChangeError = '';
    }
    const dispatchIdleItem =
      next.find((item) => item.kind === 'DispatchIdle') ?? null;
    onDispatchIdleWarning?.(dispatchIdleItem);
    if (!dispatchIdleItem) {
      idleActionBusy = {};
      idleActionError = {};
    }
  }

  function receive(
    next: ProjectHealthItem[],
    nextHistory: ProjectHealthHistoryEntry[],
    announce: boolean,
  ) {
    const menuOpen = menu?.matches(':popover-open') ?? false;
    const fresh =
      announce && !menuOpen
        ? next.filter((item) => seen.get(item.key) !== fingerprint(item))
        : [];
    seen = new Map(next.map((item) => [item.key, fingerprint(item)]));
    items = next;
    history = nextHistory;
    publishHeaderWarnings(next);
    const live = new Set(next.map((item) => item.key));
    const freshKeys = new Set(fresh.map((item) => item.key));
    const kept = popouts.filter(
      (item) => live.has(item.key) && !freshKeys.has(item.key),
    );
    for (const item of popouts) if (!live.has(item.key)) stopTimer(item.key);
    popouts = [...fresh, ...kept].slice(0, MAX_POPOUTS);
    fresh.forEach((item) => startTimer(item.key));
  }

  function startTimer(key: string) {
    stopTimer(key);
    timers.set(
      key,
      setTimeout(() => dismiss(key), POPOUT_MS),
    );
  }

  function stopTimer(key: string) {
    clearTimeout(timers.get(key));
    timers.delete(key);
  }

  function dismiss(key: string) {
    stopTimer(key);
    popouts = popouts.filter((item) => item.key !== key);
  }

  function clearPopouts() {
    for (const key of [...timers.keys()]) stopTimer(key);
    popouts = [];
  }

  function openThread(threadId: string) {
    menu?.hidePopover();
    clearPopouts();
    send({ OpenThread: threadId });
  }

  function openActionUrl(event: globalThis.MouseEvent, url: string) {
    event.preventDefault();
    menu?.hidePopover();
    clearPopouts();
    void collapseCommands.openPage(url);
  }

  async function importCollapseIssues() {
    const id = currentProject;
    if (id === null || importingIssues) return;
    importingIssues = true;
    importIssuesError = '';
    importIssuesUrl = null;
    importIssuesUrlLabel = null;
    try {
      const step = await collapseCommands.importIssues(id);
      if (id === currentProject && step.status !== 'passed') {
        importIssuesError = step.detail
          ? `${step.summary} ${step.detail}`
          : step.summary;
        importIssuesUrl = safeExternalUrl(step.action_url);
        importIssuesUrlLabel = step.action_label ?? null;
      }
    } catch (reason) {
      if (id === currentProject) {
        importIssuesError =
          reason instanceof Error ? reason.message : String(reason);
      }
    } finally {
      if (id === currentProject) importingIssues = false;
    }
  }

  async function resolveHostSourceChange(switchBack: boolean) {
    const id = currentProject;
    if (id === null || resolvingHostChange) return;
    resolvingHostChange = true;
    hostChangeError = '';
    try {
      const view = await projectHealthCommands.resolveHostChange(
        id,
        switchBack,
      );
      if (id === currentProject && view.project_identifier === id) {
        receive(view.items, view.history ?? [], false);
      }
    } catch (reason) {
      if (id === currentProject) {
        hostChangeError =
          reason instanceof Error ? reason.message : String(reason);
      }
    } finally {
      if (id === currentProject) resolvingHostChange = false;
    }
  }

  async function runIdleIssueAction(
    issue: BlockedDispatchIssue,
    action: DispatchIdleIssueAction,
  ) {
    const id = currentProject;
    if (id === null || idleActionBusy[issue.number]) return;
    idleActionBusy = { ...idleActionBusy, [issue.number]: action };
    const nextErrors = { ...idleActionError };
    delete nextErrors[issue.number];
    idleActionError = nextErrors;
    try {
      const view = await projectHealthCommands.actOnDispatchIdleIssue(
        id,
        issue.repository,
        issue.number,
        action,
      );
      if (id === currentProject && view.project_identifier === id) {
        receive(view.items, view.history ?? [], false);
      }
    } catch (reason) {
      if (id === currentProject) {
        idleActionError = {
          ...idleActionError,
          [issue.number]:
            reason instanceof Error ? reason.message : String(reason),
        };
      }
    } finally {
      if (id === currentProject) {
        const nextBusy = { ...idleActionBusy };
        delete nextBusy[issue.number];
        idleActionBusy = nextBusy;
      }
    }
  }

  async function copyMissingIssueFromGithub(issueNumber: number) {
    const id = currentProject;
    if (id === null || copyingIssueNumber !== null) return;
    copyingIssueNumber = issueNumber;
    const nextErrors = { ...hostIssueError };
    delete nextErrors[issueNumber];
    hostIssueError = nextErrors;
    try {
      const view = await projectHealthCommands.copyGithubIssueToCollapse(
        id,
        issueNumber,
      );
      if (id === currentProject && view.project_identifier === id) {
        receive(view.items, view.history ?? [], false);
      }
    } catch (reason) {
      if (id === currentProject) {
        hostIssueError = {
          ...hostIssueError,
          [issueNumber]:
            reason instanceof Error ? reason.message : String(reason),
        };
      }
    } finally {
      if (id === currentProject) copyingIssueNumber = null;
    }
  }

  async function dismissHostIssue(issueNumber: number) {
    const id = currentProject;
    if (id === null || dismissingIssueNumber !== null) return;
    dismissingIssueNumber = issueNumber;
    const nextErrors = { ...hostIssueError };
    delete nextErrors[issueNumber];
    hostIssueError = nextErrors;
    try {
      const view = await projectHealthCommands.dismissHostIssueProblem(
        id,
        issueNumber,
      );
      if (id === currentProject && view.project_identifier === id) {
        receive(view.items, view.history ?? [], false);
      }
    } catch (reason) {
      if (id === currentProject) {
        hostIssueError = {
          ...hostIssueError,
          [issueNumber]:
            reason instanceof Error ? reason.message : String(reason),
        };
      }
    } finally {
      if (id === currentProject) dismissingIssueNumber = null;
    }
  }

  $effect(() => {
    const id = currentProject;
    items = [];
    history = [];
    seen = new Map();
    importingIssues = false;
    importIssuesError = '';
    importIssuesUrl = null;
    importIssuesUrlLabel = null;
    resolvingHostChange = false;
    hostChangeError = '';
    copyingIssueNumber = null;
    dismissingIssueNumber = null;
    hostIssueError = {};
    idleActionBusy = {};
    idleActionError = {};
    clearPopouts();
    publishHeaderWarnings([]);
    if (id === null) return;
    let stopped = false;
    // The first view, from the read or an early event, only fills the badge.
    let loaded = false;
    void projectHealthCommands
      .get(id)
      .then((view) => {
        if (stopped || loaded || view.project_identifier !== id) return;
        receive(view.items, view.history ?? [], false);
        loaded = true;
      })
      .catch(() => {
        // The next live event fills the list if the initial read fails.
      });
    const listening = onProjectHealth((event) => {
      if (stopped || event.payload.view.project_identifier !== id) return;
      receive(
        event.payload.view.items,
        event.payload.view.history ?? [],
        loaded,
      );
      loaded = true;
    });
    return () => {
      stopped = true;
      clearPopouts();
      void listening.then((unlisten) => unlisten());
    };
  });

  function formatWhen(actedAtMs: string): string {
    const ms = Number(actedAtMs);
    if (!Number.isFinite(ms) || ms <= 0) return '';
    return new Date(ms).toLocaleTimeString([], {
      hour: '2-digit',
      minute: '2-digit',
    });
  }

  function formatHistoryWindow(
    startedAtMs: string,
    endedAtMs: string | null | undefined,
  ): string {
    const start = formatWhen(startedAtMs);
    if (!start) return '';
    if (!endedAtMs) return `since ${start}`;
    const end = formatWhen(endedAtMs);
    return end ? `${start} – ${end}` : start;
  }

  const summary = $derived(
    items.length === 0
      ? 'Project health: all clear'
      : `Project health: ${items.length} signal${items.length === 1 ? '' : 's'}`,
  );
</script>

{#snippet healthItem(item: ProjectHealthItem, popout: boolean)}
  {@const actionUrl = safeExternalUrl(item.action_url)}
  <span class="health-dot" aria-hidden="true"></span>
  <div class="health-body">
    <div class="health-headline">
      <strong class="health-signal">{item.signal}</strong>
      {#if formatWhen(item.acted_at_ms)}
        <time
          class="health-when"
          datetime={new Date(Number(item.acted_at_ms)).toISOString()}
          title={new Date(Number(item.acted_at_ms)).toLocaleString()}
          >{formatWhen(item.acted_at_ms)}</time
        >
      {/if}
    </div>
    <div class="health-action">{item.action}</div>
    {#if item.detail && !popout && !(item.kind === 'DispatchIdle' && (item.dispatch_idle?.blocked_issues?.length ?? 0) > 0)}
      <div class="health-detail">{item.detail}</div>
    {/if}
    {#if item.kind === 'DispatchIdle' && item.dispatch_idle && (item.dispatch_idle.blocked_issues?.length ?? 0) > 0 && !popout && currentProject !== null}
      {@const blockedIssues = item.dispatch_idle.blocked_issues ?? []}
      <ul class="health-blocked-list">
        {#each blockedIssues as issue (issue.number)}
          {@const issueUrl = safeExternalUrl(issue.issue_url)}
          {@const busy = idleActionBusy[issue.number]}
          <li class="health-blocked-row">
            <div class="health-blocked-summary">
              <strong class="health-blocked-num">#{issue.number}</strong>
              <span class="health-blocked-reason" title={issue.title}
                >{issue.reason}</span
              >
            </div>
            {#if issue.last_attempt_result}
              <div class="health-detail">
                Last attempt: {issue.last_attempt_result}
              </div>
            {/if}
            <div class="health-actions">
              {#if issue.can_lift_hold}
                <button
                  class="mini health-open"
                  type="button"
                  disabled={Boolean(busy)}
                  onclick={() => void runIdleIssueAction(issue, 'lift_hold')}
                  >{busy === 'lift_hold' ? 'Lifting…' : 'Lift hold'}</button
                >
              {/if}
              {#if issue.can_retry_attempt}
                <button
                  class="mini health-open"
                  type="button"
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
                  class="mini health-open"
                  type="button"
                  disabled={Boolean(busy)}
                  onclick={() =>
                    void runIdleIssueAction(issue, 'mark_needs_owner')}
                  >{busy === 'mark_needs_owner'
                    ? 'Labelling…'
                    : 'Needs owner'}</button
                >
              {/if}
              {#if issue.last_attempt_thread}
                {@const attemptThread = issue.last_attempt_thread}
                <button
                  class="mini health-open"
                  type="button"
                  onclick={() => openThread(attemptThread)}>Open thread</button
                >
              {/if}
              {#if issueUrl}
                <a
                  class="mini health-open"
                  href={issueUrl}
                  target="_blank"
                  rel="noopener noreferrer"
                  onclick={(event) => openActionUrl(event, issueUrl)}
                  >Open #{issue.number} ↗</a
                >
              {/if}
            </div>
            {#if idleActionError[issue.number]}
              <div class="health-detail" role="alert">
                {idleActionError[issue.number]}
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
    {#if item.thread_identifier}
      {@const threadId = item.thread_identifier}
      <button
        class="mini health-open"
        type="button"
        onclick={() => openThread(threadId)}>Open thread</button
      >
    {/if}
    {#if item.kind === 'HostSourceChanged' && item.host_switch_back && currentProject !== null}
      {@const switchBack = item.host_switch_back}
      <div class="health-actions">
        {#if switchBack.can_switch_back}
          <button
            class="mini health-open"
            type="button"
            disabled={resolvingHostChange}
            onclick={() => void resolveHostSourceChange(true)}
            >{resolvingHostChange
              ? 'Switching…'
              : switchBack.button_label}</button
          >
        {/if}
        <button
          class="mini health-open"
          type="button"
          disabled={resolvingHostChange}
          onclick={() => void resolveHostSourceChange(false)}
          >{switchBack.keep_label}</button
        >
      </div>
      {#if !popout && hostChangeError}
        <div class="health-detail" role="alert">{hostChangeError}</div>
      {/if}
    {/if}
    {#if (item.kind === 'HostIssueMissing' || item.kind === 'HostIssueMismatch') && item.host_issue_action && currentProject !== null}
      {@const issueAction = item.host_issue_action}
      <div class="health-actions">
        {#if issueAction.can_copy_from_github}
          <button
            class="mini health-open"
            type="button"
            disabled={copyingIssueNumber === issueAction.issue_number ||
              dismissingIssueNumber === issueAction.issue_number}
            onclick={() =>
              void copyMissingIssueFromGithub(issueAction.issue_number)}
            >{copyingIssueNumber === issueAction.issue_number
              ? 'Copying…'
              : 'Copy from GitHub'}</button
          >
        {/if}
        {#if issueAction.can_dismiss}
          <button
            class="mini health-open"
            type="button"
            disabled={copyingIssueNumber === issueAction.issue_number ||
              dismissingIssueNumber === issueAction.issue_number}
            onclick={() => void dismissHostIssue(issueAction.issue_number)}
            >{dismissingIssueNumber === issueAction.issue_number
              ? 'Dismissing…'
              : 'Dismiss'}</button
          >
        {/if}
      </div>
      {#if !popout && hostIssueError[issueAction.issue_number]}
        <div class="health-detail" role="alert">
          {hostIssueError[issueAction.issue_number]}
        </div>
      {/if}
    {/if}
    {#if item.kind === 'CollapseIssuesNotImported' && currentProject !== null}
      <button
        class="mini health-open"
        type="button"
        disabled={importingIssues}
        onclick={() => void importCollapseIssues()}
        >{importingIssues ? 'Importing…' : 'Import now'}</button
      >
      {#if !popout && importIssuesError}
        <div class="health-detail" role="alert">{importIssuesError}</div>
      {/if}
      {#if !popout && importIssuesUrl}
        <a
          class="mini health-open"
          href={importIssuesUrl}
          target="_blank"
          rel="noopener noreferrer"
          onclick={(event) => openActionUrl(event, importIssuesUrl!)}
          >{importIssuesUrlLabel ?? 'Open'} ↗</a
        >
      {/if}
    {/if}
    {#if actionUrl}
      <a
        class="mini health-open"
        href={actionUrl}
        target="_blank"
        rel="noopener noreferrer"
        onclick={(event) => openActionUrl(event, actionUrl)}
        >{item.action_label ?? 'Open'} ↗</a
      >
    {/if}
  </div>
{/snippet}

<button
  class="icon-btn health-bell"
  type="button"
  popovertarget="health-menu"
  aria-haspopup="dialog"
  data-severity={worst}
  title={summary}
  aria-label={summary}
  onclick={clearPopouts}
  ><Icon name="bell" />{#if items.length > 0}<span
      class="health-count"
      aria-hidden="true">{items.length}</span
    >{/if}</button
>

<div
  bind:this={menu}
  id="health-menu"
  class="health-menu"
  popover="auto"
  role="dialog"
  aria-label="Project health"
>
  <div class="health-menu-head">Project health</div>
  {#each items as item (item.key)}
    <div class="health-row" data-severity={item.severity}>
      {@render healthItem(item, false)}
    </div>
  {:else}
    <p class="health-empty">All clear. Velocity reports problems here.</p>
  {/each}
  {#if history.length > 0}
    <div class="health-menu-head health-history-head">History</div>
    {#each history as entry (entry.id)}
      <div class="health-row health-history-row" data-severity={entry.severity}>
        <span class="health-dot" aria-hidden="true"></span>
        <div class="health-body">
          <div class="health-headline">
            <strong class="health-signal">{entry.summary}</strong>
            {#if formatHistoryWindow(entry.started_at_ms, entry.ended_at_ms)}
              <span
                class="health-when"
                title={new Date(Number(entry.started_at_ms)).toLocaleString()}
                >{formatHistoryWindow(
                  entry.started_at_ms,
                  entry.ended_at_ms,
                )}</span
              >
            {/if}
          </div>
          {#if entry.detail}
            <div class="health-detail">{entry.detail}</div>
          {/if}
        </div>
      </div>
    {/each}
  {/if}
</div>

{#if popouts.length > 0}
  <div class="health-popouts" role="status" aria-live="polite">
    {#each popouts as item (item.key)}
      <div
        class="health-row health-popout"
        data-severity={item.severity}
        role="group"
        aria-label={item.signal}
        onpointerenter={() => stopTimer(item.key)}
        onpointerleave={() => startTimer(item.key)}
      >
        {@render healthItem(item, true)}
        <button
          class="health-dismiss"
          type="button"
          aria-label="Dismiss"
          title="Dismiss"
          onclick={() => dismiss(item.key)}>×</button
        >
      </div>
    {/each}
  </div>
{/if}

<style>
  .health-bell {
    position: relative;
    color: var(--muted);
  }
  .health-bell[data-severity='Info'] {
    color: var(--info);
  }
  .health-bell[data-severity='Warning'] {
    color: var(--warn);
  }
  .health-bell[data-severity='Error'] {
    color: var(--err);
  }
  .health-count {
    position: absolute;
    top: 0;
    right: 0;
    min-width: 14px;
    height: 14px;
    padding: 0 var(--sp-1);
    border-radius: var(--r-pill);
    /* The digit sits on the severity colour, so it takes the window black. */
    background: var(--muted);
    color: var(--bg);
    font-size: 10px;
    font-weight: var(--fw-semibold);
    line-height: 14px;
    text-align: center;
    font-variant-numeric: tabular-nums;
  }
  .health-bell[data-severity='Info'] .health-count {
    background: var(--info);
  }
  .health-bell[data-severity='Warning'] .health-count {
    background: var(--warn);
  }
  .health-bell[data-severity='Error'] .health-count {
    background: var(--err);
  }

  .health-menu,
  .health-popout {
    border: 1px solid var(--pop-edge);
    background: var(--glass-pop);
    box-shadow: var(--shadow-pop);
    backdrop-filter: var(--blur-pop);
    -webkit-backdrop-filter: var(--blur-pop);
    color: var(--text-2);
    font-size: var(--fs-sm);
    line-height: var(--lh);
  }
  .health-menu {
    position: fixed;
    inset: var(--titlebar-h) var(--gutter) auto auto;
    width: min(440px, calc(100vw - 2 * var(--gutter)));
    max-height: min(70vh, 560px);
    margin: 0;
    padding: var(--sp-3);
    overflow-y: auto;
    border-radius: var(--r-lg);
  }
  .health-menu:popover-open {
    display: grid;
    gap: var(--sp-1);
  }
  .health-menu-head {
    padding: var(--sp-2) var(--sp-4) var(--sp-3);
    color: var(--muted);
    font-size: var(--fs-xs);
    font-weight: var(--fw-semibold);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .health-empty {
    margin: 0;
    padding: var(--sp-4);
    color: var(--muted);
  }
  .health-menu .health-row {
    padding: var(--sp-4);
    border-radius: var(--r-md);
  }
  .health-menu .health-row:hover {
    background: var(--surface-2);
  }

  .health-popouts {
    position: fixed;
    top: calc(var(--titlebar-h) + var(--sp-2));
    right: var(--gutter);
    z-index: 40;
    display: grid;
    gap: var(--sp-3);
    width: min(380px, calc(100vw - 2 * var(--gutter)));
    pointer-events: none;
  }
  .health-popout {
    position: relative;
    padding: var(--sp-4) var(--sp-8) var(--sp-4) var(--sp-5);
    border-radius: var(--r-md);
    pointer-events: auto;
    animation: health-pop-in var(--motion-duration-base) var(--motion-ease-out);
  }
  .health-dismiss {
    position: absolute;
    top: var(--sp-2);
    right: var(--sp-3);
    color: var(--muted);
    font-size: var(--fs-md);
    line-height: 1;
  }
  .health-dismiss:hover {
    color: var(--text);
  }
  @keyframes health-pop-in {
    from {
      opacity: 0;
      transform: translateY(calc(-1 * var(--sp-3)))
        scale(var(--motion-scale-from));
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .health-popout {
      animation: none;
    }
  }

  .health-row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-4);
    min-width: 0;
  }
  .health-dot {
    flex: none;
    width: var(--sp-3);
    height: var(--sp-3);
    margin-top: var(--sp-2);
    border-radius: var(--r-pill);
    background: var(--info);
  }
  .health-row[data-severity='Warning'] .health-dot {
    background: var(--warn);
  }
  .health-row[data-severity='Error'] .health-dot {
    background: var(--err);
  }
  .health-body {
    display: grid;
    gap: var(--sp-1);
    flex: 1;
    min-width: 0;
  }
  .health-headline {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-4);
  }
  .health-signal {
    color: var(--text);
    font-weight: var(--fw-semibold);
  }
  .health-row[data-severity='Warning'] .health-signal {
    color: var(--warn);
  }
  .health-row[data-severity='Error'] .health-signal {
    color: var(--err);
  }
  .health-when {
    flex: none;
    color: var(--muted);
    font-size: var(--fs-xs);
    font-variant-numeric: tabular-nums;
  }
  .health-action {
    color: var(--text-2);
  }
  .health-detail {
    color: var(--muted);
    font-family: var(--mono);
    font-size: var(--fs-xs);
    overflow-wrap: anywhere;
  }
  .health-open {
    justify-self: start;
    margin-top: var(--sp-2);
  }
  .health-actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .health-blocked-list {
    display: grid;
    gap: var(--sp-3);
    margin: var(--sp-2) 0 0;
    padding: 0;
    list-style: none;
  }
  .health-blocked-row {
    display: grid;
    gap: var(--sp-1);
    padding: var(--sp-3);
    border: 1px solid var(--line-dim);
    border-radius: var(--r-sm);
    background: var(--card);
  }
  .health-blocked-summary {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--sp-2);
  }
  .health-blocked-num {
    color: var(--text);
    font-weight: var(--fw-semibold);
    font-variant-numeric: tabular-nums;
  }
  .health-blocked-reason {
    color: var(--text-2);
    overflow-wrap: anywhere;
  }
  .health-history-head {
    margin-top: var(--sp-2);
    border-top: 1px solid var(--line-dim);
    padding-top: var(--sp-3);
  }
  .health-history-row .health-signal {
    color: var(--text-2);
    font-weight: var(--fw-medium);
  }
</style>
