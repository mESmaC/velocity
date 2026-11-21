<script lang="ts">
  import PaneState from './PaneState.svelte';
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { rise } from './motion';
  import {
    issueCommands,
    onIssuePanel,
    onProjectSelected,
    type IssuePanelSnapshot,
    type LayoutAction,
    type LayoutSnapshot,
  } from './ipc';
  import './theme/issues.css';

  let {
    layout,
    send,
  }: { layout: LayoutSnapshot; send: (action: LayoutAction) => void } =
    $props();
  let panel = $state<IssuePanelSnapshot | null>(null);
  let error = $state('');
  let active = false;
  let projectIdentifier: number | null | undefined;
  let generation = 0;

  function accept(snapshot: IssuePanelSnapshot) {
    if (
      !active ||
      (projectIdentifier !== undefined &&
        snapshot.project_identifier !== projectIdentifier)
    )
      return;
    if (panel && snapshot.revision < panel.revision) return;
    panel = snapshot;
    error = '';
  }

  async function refresh() {
    const request = ++generation;
    try {
      const snapshot = await issueCommands.panel();
      if (request === generation) accept(snapshot);
    } catch (reason) {
      if (active && request === generation) error = String(reason);
    }
  }

  async function act(number: number, action: 'open' | 'dispatch' | 'accept') {
    const identifier = panel?.project_identifier;
    if (identifier == null) return;
    const request = generation;
    try {
      if (action === 'open') await issueCommands.open(identifier, number);
      else
        await issueCommands.dispatch(identifier, number, action === 'accept');
    } catch (reason) {
      if (active && request === generation) error = String(reason);
    }
  }

  async function releaseHold(number: number) {
    const identifier = panel?.project_identifier;
    const repository = panel?.repository;
    if (identifier == null || !repository) return;
    const request = generation;
    try {
      const snapshot = await issueCommands.releaseHold(
        identifier,
        repository,
        number,
      );
      if (request === generation) accept(snapshot);
    } catch (reason) {
      if (active && request === generation) error = String(reason);
    }
  }

  async function toggle(number: number) {
    const request = generation;
    try {
      const snapshot = await issueCommands.toggleExpanded(number);
      if (request === generation) accept(snapshot);
    } catch (reason) {
      if (active && request === generation) error = String(reason);
    }
  }

  // Disclosure keys, as in a tree: → opens a collapsed row, ← closes an open one.
  function onRowKey(event: KeyboardEvent, number: number, expanded: boolean) {
    if (
      (event.key === 'ArrowRight' && !expanded) ||
      (event.key === 'ArrowLeft' && expanded)
    ) {
      event.preventDefault();
      void toggle(number);
    }
  }

  onMount(() => {
    active = true;
    const stops: (() => void)[] = [];
    async function start() {
      const stopPanel = await onIssuePanel((event) =>
        accept(event.payload.panel),
      );
      if (!active) {
        stopPanel();
        return;
      }
      stops.push(stopPanel);
      const stopProject = await onProjectSelected((event) => {
        projectIdentifier = event.payload.selection.active_project;
        panel = null;
        error = '';
        void refresh();
      });
      if (!active) {
        stopProject();
        return;
      }
      stops.push(stopProject);
      await refresh();
    }
    void start().catch((reason: unknown) => {
      if (active) error = String(reason);
    });
    return () => {
      active = false;
      generation += 1;
      stops.forEach((stop) => stop());
    };
  });
</script>

<section class="panel issue-panel" aria-label="Open Issues">
  <div class="panel-h">
    <span class="label">Open Issues</span>
    <div class="panel-actions">
      <span class="count">{panel?.rows.length || ''}</span>
      <button
        class="swarm-button"
        onclick={() => send({ Request: 'SwarmIssues' })}
        ><Icon name="swarm" />Swarm</button
      >
    </div>
  </div>
  <div class="panel-body" aria-busy={!panel || panel.loading}>
    <PaneState
      name="Issues"
      loading={!panel || panel.loading}
      empty={!panel?.rows.length}
      error={error || panel?.error || ''}
      retry={refresh}
      message={panel?.project_identifier == null
        ? 'Select a project.'
        : panel.repository === ''
          ? 'Local project · no GitHub remote.'
          : 'No open issues.'}
    />
    {#each panel?.rows ?? [] as row, index (row.number)}
      {#if row.question && index === 0}<div class="issue-group label">
          Questions
        </div>{/if}
      {#if !row.question && index > 0 && panel?.rows[index - 1]?.question}<div
          class="issue-group label"
        >
          Issues
        </div>{/if}
      {@const expanded = panel?.expanded.includes(row.number) ?? false}
      <div
        class="issue-row"
        class:taken={row.thread_identifier !== null}
        class:expanded
        in:rise
      >
        <button
          class="item issue-item"
          title={row.title}
          aria-expanded={expanded}
          aria-controls="issue-detail-{row.number}"
          onclick={() => toggle(row.number)}
          onkeydown={(event) => onRowKey(event, row.number, expanded)}
        >
          <Icon name="issue" /><span class="item-title">{row.title}</span><span
            class="count issue-number">#{row.number}</span
          >
          <span class="item-meta issue-meta">
            {#if row.thread_label}<span class="issue-agent"
                >{row.thread_label}</span
              >{/if}
            {#if row.blocked_by}
              <span class="issue-blocker">Blocked by {row.blocked_by}</span>
            {/if}
            {#if row.upstream_wait}
              <span class="issue-blocker">Upstream · {row.upstream_wait}</span>
            {/if}
            {#if row.tracking}
              <span class="issue-tracking"
                >Tracking {row.tracking.checked}/{row.tracking.total}</span
              >
            {/if}
            <span>{row.labels.join(' · ')}</span>
            {#if row.scout}<span class="issue-scout"
                ><Icon name="scout" />{row.scout}</span
              >{/if}
          </span>
        </button>
        {#if row.thread_identifier === null}
          <button
            class="dispatch"
            aria-label={row.question
              ? `Accept recommendation for issue ${row.number}`
              : `Dispatch issue ${row.number}`}
            disabled={!layout.build_allowed ||
              (row.question !== null && !row.question.recommendation)}
            title={layout.build_block_reason ??
              (row.question
                ? 'Open a SPEC.md change with this answer'
                : 'Spin up an agent on this issue')}
            onclick={() =>
              act(row.number, row.question ? 'accept' : 'dispatch')}
            ><Icon name="play" />{row.question ? 'Accept' : 'Dispatch'}</button
          >
        {/if}
        {#if row.person_hold}
          <div class="work-hold-row">
            <span
              class="work-hold-text"
              title={`${row.person_hold.reason} · ${row.person_hold.release_action}`}
              >{row.person_hold.reason} · {row.person_hold.release_action}</span
            >
            <button
              class="mini work-hold-release"
              type="button"
              onclick={() => void releaseHold(row.number)}>Release</button
            >
          </div>
        {/if}
        {#if expanded}
          <div class="issue-detail" id="issue-detail-{row.number}" in:rise>
            {#if row.question}
              <p class="issue-summary">
                {row.question.recommendation ||
                  'No recommended answer recorded.'}
              </p>
              <p class="issue-blocks">
                Blocks {row.question.blocked_items} item{row.question
                  .blocked_items === 1
                  ? ''
                  : 's'}
              </p>
            {:else}
              <p class="issue-summary">
                {row.summary || 'No description.'}
              </p>
            {/if}
            <button
              class="mini"
              type="button"
              onclick={() => act(row.number, 'open')}
              >{row.thread_identifier === null
                ? 'Open on GitHub'
                : 'Open thread'}</button
            >
          </div>
        {/if}
      </div>
    {/each}
  </div>
</section>
