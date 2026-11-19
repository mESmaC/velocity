<script lang="ts">
  import Icon from './Icon.svelte';
  import WorkItemDetails from './WorkItemDetails.svelte';
  import {
    issueCommands,
    type IssueDiscussion,
    type LayoutAction,
  } from './ipc';
  import { rise } from './motion';
  import { disclosureKey, workState, type WorkIssue } from './work-panel';

  let {
    issue,
    project,
    expanded,
    blocked,
    send,
    report,
  }: {
    issue: WorkIssue;
    project: number;
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
    const timer = window.setInterval(load, 30_000);
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
      <span class="work-source">GitHub issue</span>
      <span class="issue-agent"
        >{workState(issue.item, owner === null ? 'Todo' : 'Claimed')}</span
      >
      {#if row?.blocked_by}<span class="issue-blocker"
          >Blocked by {row.blocked_by}</span
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
  <div class="issue-links">
    <WorkItemDetails
      {project}
      item={issue.item}
      owner={owner?.toString() ?? null}
      {send}
    />
    <button class="task-source" type="button" onclick={discussion}
      >GitHub discussion</button
    >
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
    padding: var(--layout-item-padding);
    padding-left: var(--issue-detail-indent);
  }
  .agent-comment {
    display: grid;
    gap: var(--layout-item-gap);
    padding: var(--layout-item-padding);
    border-radius: var(--r-sm);
    background: var(--surface-2);
    overflow-wrap: anywhere;
  }
  .agent-comment p {
    white-space: pre-wrap;
  }
</style>
