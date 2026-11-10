<script lang="ts">
  import { onMount } from 'svelte';
  import {
    onProjectSelected,
    onPullRequestPanel,
    pullRequestCommands,
    type PullRequestPanelSnapshot,
  } from './ipc';
  import './theme/pull-requests.css';

  let panel = $state<PullRequestPanelSnapshot | null>(null);
  let error = $state('');
  let projectIdentifier: number | null | undefined;
  let active = false;
  let generation = 0;

  function accept(snapshot: PullRequestPanelSnapshot) {
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
      const snapshot = await pullRequestCommands.panel();
      if (request === generation) accept(snapshot);
    } catch (reason) {
      if (active && request === generation) error = String(reason);
    }
  }

  async function open(number: number) {
    const identifier = panel?.project_identifier;
    if (identifier == null) return;
    const request = generation;
    try {
      await pullRequestCommands.open(identifier, number);
    } catch (reason) {
      if (active && request === generation) error = String(reason);
    }
  }

  onMount(() => {
    active = true;
    const stops: (() => void)[] = [];
    async function start() {
      const stopPanel = await onPullRequestPanel((event) =>
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

<section class="panel pull-request-panel" aria-label="Open Pull Requests">
  <div class="panel-h">
    <span class="label">Open Pull Requests</span>
    <span class="count">{panel?.rows.length || ''}</span>
  </div>
  <div class="panel-body" aria-busy={!panel || panel.loading}>
    {#each panel?.rows ?? [] as row (row.number)}
      <button
        class="item pull-request-row"
        title={row.title}
        onclick={() => open(row.number)}
      >
        <span
          class="pull-request-check"
          data-status={row.checks}
          role="img"
          aria-label={`CI ${row.checks}`}
        ></span>
        <span class="item-title">{row.title}</span>
        <span class="count">#{row.number}</span>
        <span class="item-meta pull-request-meta">
          <span class="pull-request-add">+{row.additions}</span>
          <span class="pull-request-delete">−{row.deletions}</span>
          {#if row.tier}<span
              class="pull-request-tier"
              data-tier={row.tier}
              title="Review tier">{row.tier}</span
            >{/if}
          <span class="pull-request-review" data-status={row.review}
            >{row.review_label}</span
          >
          {#if row.reviewer}<span class="pull-request-reviewer"
              >{row.reviewer}</span
            >{/if}
        </span>
      </button>
    {:else}
      {#if !error && !panel?.error}
        <p class="empty">
          {!panel || panel.loading
            ? 'Loading pull requests…'
            : 'No open pull requests.'}
        </p>
      {/if}
    {/each}
    {#if error || panel?.error}
      <p class="empty pull-request-error" role="status">
        {error || panel?.error}
      </p>
    {/if}
  </div>
</section>
