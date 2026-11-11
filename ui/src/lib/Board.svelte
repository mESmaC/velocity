<script lang="ts">
  import Icon from './Icon.svelte';
  import type { BoardCard, LayoutAction, LayoutSnapshot } from './ipc';
  let {
    layout,
    send,
  }: {
    layout: LayoutSnapshot;
    send: (action: LayoutAction) => void;
  } = $props();

  // The same icon per thread state as the sidebar, so a card and its row read alike.
  const icons: Record<string, string> = {
    Duty: 'scout',
    Swarm: 'swarm',
    Spec: 'issue',
    Working: 'circle',
    Review: 'pr',
    Error: 'alert',
    Closed: 'circle',
    Done: 'check',
    Merged: 'check',
  };
  const emptyLane: Record<string, string> = {
    UpNext: 'No tasks or issues are waiting.',
    Running: 'No agents are running.',
    InReview: 'No pull requests are open.',
    Finished: 'Nothing has finished yet.',
    OnDuty: 'No agents are on duty.',
  };

  function cardIcon(card: BoardCard): string {
    if (card.kind === 'Task') return 'circle';
    if (card.kind === 'Issue') return 'issue';
    return icons[card.group ?? ''] ?? 'circle';
  }
</script>

<section class="board" aria-label="Board">
  {#each layout.board as lane (lane.kind)}
    <section class="lane" data-lane={lane.kind} aria-label={lane.title}>
      <header class="lane-h">
        <span class="lane-dot"></span>
        <h2>{lane.title}</h2>
        <span class="lane-count">{lane.count}</span>
        {#if lane.kind === 'UpNext' && lane.cards.length > 0}<button
            class="swarm-button"
            title="Swarm the Task List (W)"
            onclick={() => send({ Request: { Navigation: 'Swarm' } })}
            ><Icon name="swarm" />Swarm</button
          >{/if}
      </header>
      <div class="lane-cards">
        {#each lane.cards as card, index (`${card.kind}:${card.thread ?? card.title}:${index}`)}
          {@const thread = card.thread}
          {#if thread !== null}
            <button
              class="card"
              class:active={card.active}
              class:paused={card.paused}
              data-status={card.group}
              aria-current={card.active ? 'true' : undefined}
              title={card.title}
              onclick={() => send({ OpenThread: thread })}
            >
              <span class="status-icon"><Icon name={cardIcon(card)} /></span>
              <span class="card-title">{card.title}</span>
              {#if card.metadata}<span class="card-meta">{card.metadata}</span
                >{/if}
            </button>
          {:else}
            <article class="card work" title={card.title}>
              <span class="status-icon"><Icon name={cardIcon(card)} /></span>
              <span class="card-title">{card.title}</span>
              <span class="card-meta"
                >{card.kind === 'Task' ? 'task' : 'issue'}{card.metadata
                  ? ` · ${card.metadata}`
                  : ''}</span
              >
            </article>
          {/if}
        {/each}
        {#if lane.cards.length === 0}<p class="lane-empty">
            {emptyLane[lane.kind]}
          </p>{/if}
      </div>
    </section>
  {/each}
</section>
