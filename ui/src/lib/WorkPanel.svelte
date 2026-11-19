<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Icon from './Icon.svelte';
  import WorkIssueRow from './WorkIssueRow.svelte';
  import WorkItemDetails from './WorkItemDetails.svelte';
  import {
    issueCommands,
    onIssuePanel,
    type IssuePanelSnapshot,
    type LayoutAction,
    type LayoutSnapshot,
  } from './ipc';
  import { rise } from './motion';
  import {
    completion,
    disclosureKey,
    issueKey,
    workGroups,
    workState,
  } from './work-panel';
  import './theme/issues.css';

  let {
    layout,
    project,
    send,
  }: {
    layout: LayoutSnapshot;
    project: number;
    send: (action: LayoutAction) => void;
  } = $props();

  let panel = $state<IssuePanelSnapshot | null>(null);
  let error = $state('');
  let openDays = $state<string[]>([]);
  let items: globalThis.HTMLDivElement;
  let lastTask: string | undefined;
  let seenTasks = false;
  let active = false;

  const groups = $derived(
    workGroups(layout.tasks, layout.rosen_work?.items ?? [], panel),
  );
  const expanded = $derived(
    new Set(
      (layout.rosen_work?.expanded_issues ?? []).flatMap((key) =>
        key.kind === 'issue' ? [issueKey(key.repository, key.number)] : [],
      ),
    ),
  );
  const blocked = $derived(
    layout.build_allowed
      ? null
      : (layout.build_block_reason ?? 'Build unavailable'),
  );

  function accept(snapshot: IssuePanelSnapshot) {
    if (
      active &&
      snapshot.project_identifier === project &&
      (!panel || snapshot.revision >= panel.revision)
    ) {
      panel = snapshot;
    }
  }

  async function refresh() {
    error = '';
    try {
      accept(await issueCommands.panel());
    } catch (reason) {
      if (active) error = String(reason);
    }
  }

  onMount(() => {
    active = true;
    let stop: (() => void) | undefined;
    async function start() {
      const unlisten = await onIssuePanel(({ payload }) =>
        accept(payload.panel),
      );
      if (!active) {
        unlisten();
        return;
      }
      stop = unlisten;
      await refresh();
    }
    void start().catch((reason: unknown) => {
      if (active) error = String(reason);
    });
    return () => {
      active = false;
      stop?.();
    };
  });

  $effect(() => {
    const newest = layout.tasks.at(-1)?.identifier;
    if (
      seenTasks &&
      newest &&
      newest !== lastTask &&
      (lastTask === undefined || Number(newest) > Number(lastTask))
    ) {
      void tick().then(() =>
        items
          ?.querySelector(`[data-work-task="${globalThis.CSS.escape(newest)}"]`)
          ?.scrollIntoView({ block: 'nearest' }),
      );
    }
    seenTasks = true;
    lastTask = newest;
  });
</script>

{#snippet workRow(row: (typeof groups.active)[number])}
  {#if row.kind === 'task'}
    {@const task = row.task}
    {@const done = completion(row) !== undefined}
    <div
      class="item task"
      class:done
      data-work-task={task.identifier}
      title={task.title}
      in:rise
    >
      <Icon
        name={done
          ? 'check'
          : task.state === 'draft'
            ? 'dashed'
            : row.item || task.state === 'now'
              ? 'play'
              : 'circle'}
      />
      <span class="item-title">{task.title}</span>
      <div class="item-meta">
        <div class="issue-meta">
          <span class="work-source">Task</span>
          <span
            >{done && row.item?.status !== 'green'
              ? 'Done'
              : workState(
                  row.item,
                  task.state === 'draft'
                    ? 'Draft'
                    : task.state === 'now'
                      ? 'Claimed'
                      : 'Todo',
                )}</span
          >
        </div>
        <WorkItemDetails
          {project}
          item={row.item}
          owner={task.thread_identifier}
          {send}
        />
        {#if task.attribution}
          {#if task.attribution_thread}
            {@const author = task.attribution_thread}
            <button
              class="task-source"
              type="button"
              onclick={() => send({ OpenThread: author })}
              >{task.attribution}</button
            >
          {:else}
            <span>{task.attribution}</span>
          {/if}
        {/if}
      </div>
    </div>
  {:else}
    <WorkIssueRow
      issue={row}
      {project}
      expanded={expanded.has(row.key)}
      {blocked}
      {send}
      report={(message) => (error = message)}
    />
  {/if}
{/snippet}

<section class="panel work-panel" aria-label="Work">
  <div class="panel-h">
    <span class="label">Work</span>
    <div class="panel-actions">
      <span class="count" title="Completed tasks"
        >{layout.task_count_label}</span
      >
      <button
        class="swarm-button"
        type="button"
        onclick={() => send({ Request: 'SwarmWork' })}
      >
        <Icon name="swarm" />Swarm
      </button>
      <button
        aria-label="Add a task"
        title="Add a task (T)"
        type="button"
        onclick={() => send({ Request: { Navigation: 'Task' } })}
      >
        <Icon name="plus" />
      </button>
    </div>
  </div>
  <div class="panel-body task-body">
    <div class="task-items" bind:this={items}>
      {#if groups.questions.length}
        <div class="issue-group label">Questions</div>
        {#each groups.questions as row (row.key)}
          {@render workRow(row)}
        {/each}
      {/if}
      {#each groups.active as row (row.key)}
        {@render workRow(row)}
      {/each}
      {#each groups.done as group (group.key)}
        <details
          ontoggle={(event) => {
            const day = group.key;
            if (event.currentTarget.open) {
              if (!openDays.includes(day)) openDays = [...openDays, day];
            } else {
              openDays = openDays.filter((openDay) => openDay !== day);
            }
          }}
        >
          <summary
            class="issue-group label"
            onkeydown={(event) => {
              const details = event.currentTarget.parentElement;
              if (details instanceof globalThis.HTMLDetailsElement) {
                disclosureKey(
                  event,
                  details.open,
                  () => (details.open = !details.open),
                );
              }
            }}
            >Done · {group.label}
            <span class="count">{group.rows.length}</span></summary
          >
          {#if openDays.includes(group.key)}
            {#each group.rows as row (row.key)}
              {@render workRow(row)}
            {/each}
          {/if}
        </details>
      {/each}
      {#if error || panel?.error}
        <p class="empty issue-error" role="status">{error || panel?.error}</p>
        <button class="mini" type="button" onclick={refresh}
          >Refresh issues</button
        >
      {:else if !panel || panel.loading}
        <p class="empty" role="status">Loading GitHub issues…</p>
      {:else if !groups.questions.length && !groups.active.length && !groups.done.length}
        <p class="empty">No work yet.</p>
      {/if}
    </div>
    <form
      class="task-add"
      onsubmit={(event) => {
        event.preventDefault();
        send('SubmitTask');
      }}
    >
      <Icon name="plus" />
      <input
        class="bare"
        id="task-input"
        aria-label="Add a task"
        placeholder="Add a task…"
        value={layout.task_text}
        oninput={(event) => send({ TaskText: event.currentTarget.value })}
      />
      <kbd>T</kbd>
    </form>
  </div>
</section>
