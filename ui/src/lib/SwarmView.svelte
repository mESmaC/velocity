<script lang="ts">
  import Composer from './Composer.svelte';
  import Icon from './Icon.svelte';
  import type {
    LayoutAction,
    LayoutSnapshot,
    ProviderColor,
    SwarmEventKind,
    SwarmViewSnapshot,
  } from './ipc';
  import './theme/swarm.css';

  let {
    layout,
    swarm,
    send,
  }: {
    layout: LayoutSnapshot;
    swarm: SwarmViewSnapshot;
    send: (action: LayoutAction) => void;
  } = $props();

  const providerIcons: Record<ProviderColor, string> = {
    Claude: 'claude',
    Codex: 'codex',
    Antigravity: 'antigravity',
    Kimi: 'kimi',
    Custom: 'agents',
  };

  // Feed dots reuse the duty feed's colours (F7, F10).
  const eventClasses: Record<SwarmEventKind, string> = {
    you: 'you',
    opened: 'file',
    changes: 'changes',
    merged: 'merge',
    rebase: 'sweep',
    failed: 'changes',
    finished: 'merge',
  };
</script>

<section class="thread-view duty-view swarm-view" aria-label={swarm.title}>
  <header class="thread-head">
    <h1>{swarm.title}</h1>
    <div class="meta" aria-label="Swarm metadata">
      <span>{swarm.agents_label}</span>
      <span>{swarm.started_label}</span>
      {#if swarm.merged_label}<span class="add">{swarm.merged_label}</span>{/if}
    </div>
    <span class="spacer"></span>
    <div class="head-actions">
      <button
        class="btn"
        type="button"
        title="Add more open items to this swarm"
        onclick={() => send({ AddSwarmItems: swarm.identifier })}
      >
        <Icon name="plus" /> Items
      </button>
    </div>
  </header>

  <div class="transcript" aria-live="polite">
    <div class="transcript-inner">
      <div class="sw-sum">
        <div class="sw-big" aria-label="Swarm progress">
          {#each swarm.segments as stage, index (index)}<i class="sg {stage}"
            ></i>{/each}
        </div>
        <div class="sw-legend">
          {#each swarm.legend as item (item.stage)}
            <span><i class="sg {item.stage}"></i>{item.label}</span>
          {/each}
        </div>
      </div>

      <div>
        <div class="section-h">
          Agents · one per item, each in its own worktree
        </div>
        <div class="sw-grid">
          {#each swarm.cards as card (card.identifier)}
            <button
              type="button"
              class="sw-card {card.stage}"
              onclick={() => send({ OpenThread: card.identifier })}
            >
              <span class="sc-h">
                <Icon name={card.issue ? 'issue' : 'circle'} />
                <span class="ttl">{card.title}</span>
                <span class="ref">{card.reference}</span>
              </span>
              <span class="sc-track" title="work · PR · CI · review · merged">
                {#each card.track as step, index (index)}{#if index > 0}<b
                    ></b>{/if}<i class={step}></i>{/each}
              </span>
              <span class="sc-doing {card.stage}">
                {#if card.stage === 'working'}<span
                    class="working-dots"
                    aria-hidden="true"><i></i><i></i><i></i></span
                  >{/if}
                <span
                  class="doing-text"
                  style:color={card.doing === 'stalled' ||
                  card.doing.startsWith('at limit')
                    ? 'var(--warn)'
                    : undefined}>{card.doing}</span
                >
              </span>
              <span class="sc-f">
                <Icon name={providerIcons[card.provider]} />
                <span>{card.agent_label}</span>
                <span class="add">+{card.added}</span>
                <span class="del">−{card.deleted}</span>
              </span>
            </button>
          {/each}
        </div>
      </div>

      <div>
        <div class="section-h">Activity</div>
        <div class="feed">
          {#each swarm.feed as event, index (`${event.timestamp}-${event.actor}-${index}`)}
            <div class="ev {eventClasses[event.kind]}">
              <span class="ts">{event.timestamp}</span>
              <span class="who">{event.actor}</span>
              <span class="what">{event.message}</span>
            </div>
          {/each}
        </div>
      </div>
    </div>
  </div>

  <div class="dock">
    <Composer
      {layout}
      {send}
      id="swarm-composer"
      placeholder={swarm.composer_placeholder}
      modelPill={layout.default_model_pill}
    />
  </div>
</section>
