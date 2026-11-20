<script lang="ts">
  import {
    onProjectHealth,
    projectHealthCommands,
    type LayoutAction,
    type ProjectHealthItem,
  } from './ipc';

  // Project health strip (#509): shows detected health signals, the automatic action
  // Velocity took (or why it could not act), and when it acted. Renders nothing while
  // every signal is clear.
  let {
    project = null,
    send,
  }: {
    project: number | null;
    send: (action: LayoutAction) => void;
  } = $props();

  let items = $state.raw<ProjectHealthItem[]>([]);
  const currentProject = $derived(project);

  $effect(() => {
    const id = currentProject;
    items = [];
    if (id === null) return;
    let stopped = false;
    void projectHealthCommands
      .get(id)
      .then((view) => {
        if (!stopped && view.project_identifier === id) {
          items = view.items;
        }
      })
      .catch(() => {
        // The next live event repopulates the strip if the initial read fails.
      });
    const listening = onProjectHealth((event) => {
      if (!stopped && event.payload.view.project_identifier === id) {
        items = event.payload.view.items;
      }
    });
    return () => {
      stopped = true;
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
</script>

{#if items.length > 0}
  <section class="health-strip" role="status" aria-label="Project health">
    {#each items as item (item.key)}
      <div class="health-row" data-severity={item.severity}>
        <span class="health-dot" aria-hidden="true"></span>
        <div class="health-body">
          <div class="health-headline">
            <strong class="health-signal">{item.signal}</strong>
            <span class="health-sep" aria-hidden="true">·</span>
            <span class="health-action">{item.action}</span>
            {#if formatWhen(item.acted_at_ms)}
              <time
                class="health-when"
                datetime={new Date(Number(item.acted_at_ms)).toISOString()}
                title={new Date(Number(item.acted_at_ms)).toLocaleString()}
                >{formatWhen(item.acted_at_ms)}</time
              >
            {/if}
          </div>
          {#if item.detail}
            <div class="health-detail">{item.detail}</div>
          {/if}
        </div>
        {#if item.thread_identifier}
          {@const threadId = item.thread_identifier}
          <button
            class="mini"
            type="button"
            onclick={() => send({ OpenThread: threadId })}>Open thread</button
          >
        {/if}
      </div>
    {/each}
  </section>
{/if}

<style>
  .health-strip {
    display: grid;
    gap: var(--sp-2);
    padding: var(--sp-3) var(--sp-6);
    border-bottom: 1px solid var(--line-dim);
    background: var(--surface);
    font-size: var(--fs-sm);
    line-height: var(--lh);
    color: var(--text-2);
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
    flex: 1;
    min-width: 0;
  }
  .health-headline {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--sp-2);
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
  .health-sep {
    color: var(--faint);
  }
  .health-action {
    color: var(--text-2);
  }
  .health-when {
    color: var(--muted);
    font-size: var(--fs-xs);
    font-variant-numeric: tabular-nums;
  }
  .health-detail {
    color: var(--muted);
    font-family: var(--mono);
    font-size: var(--fs-xs);
    overflow-wrap: anywhere;
  }
</style>
