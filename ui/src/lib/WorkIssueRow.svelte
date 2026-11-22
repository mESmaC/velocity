<script lang="ts">
  import Icon from './Icon.svelte';
  import WorkItemDetails from './WorkItemDetails.svelte';
  import {
    issueCommands,
    type IssueDiscussion,
    type IssueHost,
    type LayoutAction,
  } from './ipc';
  import { rise } from './motion';
  import { disclosureKey, workState, type WorkIssue } from './work-panel';

  let {
    issue,
    project,
    host = 'github',
    expanded,
    blocked,
    send,
    report,
  }: {
    issue: WorkIssue;
    project: number;
    host?: IssueHost;
    expanded: boolean;
    blocked: string | null;
    send: (action: LayoutAction) => void;
    report: (message: string) => void;
  } = $props();

  const number = $derived(issue.number);
  const repository = $derived(issue.repository);
  const detailsId = $derived(`work-issue-detail-${issue.key}`);
  const row = $derived(issue.row);
  const question = $derived(row?.question);
  const owner = $derived(row?.thread_identifier ?? null);
  const hold = $derived(row?.hold ?? null);
  const canDispatch = $derived(row && !issue.item && owner === null);
  let details = $state<IssueDiscussion>();
  const summary = $derived(details?.summary || row?.summary);
  let commentError = $state('');
  let refreshComments = $state(0);
  let dispatching = $state(false);

  $effect(() => {
    if (!expanded) return;
    const projectIdentifier = project;
    const issueRepository = repository;
    const issueNumber = number;
    const refresh = refreshComments;
    let stopped = false;
    let loading = false;
    async function load() {
      if (loading) return;
      loading = true;
      try {
        const values = await issueCommands.discussion(
          projectIdentifier,
          issueRepository,
          issueNumber,
        );
        if (!stopped) {
          details = values;
          commentError = '';
        }
      } catch (reason) {
        if (!stopped) commentError = String(reason);
      } finally {
        loading = false;
      }
    }
    if (refresh > 0) commentError = '';
    void load();
    // Backend caches discussions by issue updated_at; keep a slow fallback poll.
    const timer = window.setInterval(load, 120_000);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  });

  function toggle() {
    void issueCommands
      .setWorkExpanded(project, repository, number, !expanded)
      .catch((reason: unknown) => report(String(reason)));
  }

  async function dispatch() {
    if (dispatching) return;
    dispatching = true;
    try {
      await issueCommands.dispatchWork(project, repository, number, !!question);
    } catch (reason) {
      report(String(reason));
    } finally {
      dispatching = false;
    }
  }

  function discussion() {
    void issueCommands
      .openDiscussion(repository, number)
      .catch((reason: unknown) => report(String(reason)));
  }
</script>

<div class="issue-row" class:taken={!canDispatch} class:expanded in:rise>
  <button
    class="item issue-item"
    type="button"
    title={issue.title}
    aria-expanded={expanded}
    aria-controls={detailsId}
    onclick={toggle}
    onkeydown={(event) => disclosureKey(event, expanded, toggle)}
  >
    <Icon name="issue" />
    <span class="item-title">{issue.title}</span>
    <span class="count issue-number">#{number}</span>
    <span class="item-meta issue-meta">
      <span class="tag work-source">Issue</span>
      <span class="tag issue-agent" data-tone="swarm"
        >{workState(issue.item, owner === null ? 'Todo' : 'Claimed')}</span
      >
      {#if row?.blocked_by}<span class="issue-blocker"
          >Blocked by {row.blocked_by}</span
        >{/if}
      {#if row?.upstream_wait}<span
          class="issue-blocker"
          title="Checked daily by continuous dispatch; returns automatically when it ships"
          >Upstream · {row.upstream_wait}</span
        >{/if}
      {#if row?.tracking}<span
          class="issue-tracking"
          title="Tracking issue — closes automatically when every checklist item is checked"
          >Tracking {row.tracking.checked}/{row.tracking.total}</span
        >{/if}
      {#if hold}<span class="issue-blocker" title={hold.reason}
          >{hold.label}</span
        >{/if}
      {#if row?.unblocks}<span
          class="issue-unblocks"
          title={`${row.unblocks} open ${row.unblocks === 1 ? 'issue waits' : 'issues wait'} on this one. Continuous dispatch starts the issue that unblocks the most work first.`}
          >Unblocks {row.unblocks}</span
        >{/if}
      {#if row?.labels.length}<span>{row.labels.join(' · ')}</span>{/if}
      {#if row?.scout}
        <span class="issue-scout"><Icon name="scout" />{row.scout}</span>
      {/if}
    </span>
  </button>
  {#if canDispatch && !question}
    <button
      class="dispatch"
      type="button"
      aria-label={`Dispatch issue ${number}`}
      disabled={blocked !== null || dispatching}
      title={blocked ?? 'Dispatch an agent on this issue'}
      onclick={dispatch}><Icon name="play" />Dispatch</button
    >
  {/if}
  {#if hold}
    <div class="issue-hold">
      <span>{hold.reason}</span>
      {#each hold.paths as path (path)}
        <span class="issue-hold-path">{path}</span>
      {/each}
    </div>
  {/if}
  <div class="issue-links">
    <WorkItemDetails
      {project}
      item={issue.item}
      owner={owner?.toString() ?? null}
      {send}
    />
  </div>
  {#if expanded}
    <div class="issue-detail" id={detailsId} in:rise>
      {#if question}
        <p class="issue-summary">
          {question.recommendation || 'No recommended answer recorded.'}
        </p>
        <p class="issue-blocks">
          Blocks {question.blocked_items}
          {question.blocked_items === 1 ? 'item' : 'items'}
        </p>
        {#if canDispatch && question.recommendation}
          <button
            class="mini"
            type="button"
            disabled={blocked !== null || dispatching}
            title={blocked ?? 'Record the proposed answer in a Rosen change'}
            onclick={dispatch}>Accept</button
          >
        {/if}
      {:else if summary}
        <p class="issue-summary">{summary}</p>
      {/if}
      <button class="mini ghost" type="button" onclick={discussion}
        >{host === 'collapse'
          ? 'Open on Collapse ↗'
          : 'Open the GitHub discussion ↗'}</button
      >
      {#each details?.comments ?? [] as comment (comment.id)}
        <div class="agent-comment">
          <span class="issue-agent">{comment.author}</span>
          <p class="issue-summary">{comment.body}</p>
        </div>
      {/each}
      {#if commentError}
        <span class="issue-error">Could not load agent comments.</span>
        <button
          class="mini"
          type="button"
          title={commentError}
          onclick={() => (refreshComments += 1)}>Retry comments</button
        >
      {:else if !details}
        <span class="count" role="status">Loading agent comments…</span>
      {/if}
    </div>
  {/if}
</div>

<style>
  .issue-links {
    padding: 0 var(--sp-4) var(--sp-2) calc(var(--sp-4) + 16px + var(--sp-4));
    font-size: var(--fs-xs);
  }
  /* Only rows with a thread or a change have links; the rest take no extra line. */
  .issue-links:not(:has(*)) {
    display: none;
  }
  .agent-comment {
    display: grid;
    gap: var(--sp-2);
    width: 100%;
    padding: var(--sp-4) var(--sp-5);
    border: 1px solid var(--line-dim);
    border-radius: var(--r-md);
    background: var(--card);
    overflow-wrap: anywhere;
  }
  .agent-comment p {
    white-space: pre-wrap;
  }
</style>
