<script lang="ts">
  import Composer from './Composer.svelte';
  import Icon from './Icon.svelte';
  import { goalSwarmCommands } from './ipc';
  import type {
    BoardPieceKind,
    GoalClaimsBoard,
    GoalEventKind,
    GoalSwarmViewSnapshot,
    LayoutAction,
    LayoutSnapshot,
    NickTone,
    ProviderColor,
  } from './ipc';
  import './theme/swarm.css';
  import './theme/airc.css';
  import './theme/goal-swarm-view.css';

  let {
    layout,
    swarm,
    send,
  }: {
    layout: LayoutSnapshot;
    swarm: GoalSwarmViewSnapshot;
    send: (action: LayoutAction) => void;
  } = $props();

  let board = $state<GoalClaimsBoard | null>(null);
  let boardError = $state('');
  // Answers can arrive out of order; only the newest read is shown.
  let boardRequest = 0;

  const providerIcons: Record<ProviderColor, string> = {
    Claude: 'claude',
    Codex: 'codex',
    Antigravity: 'antigravity',
    Kimi: 'kimi',
    Custom: 'agents',
  };

  const pieceIcons: Record<BoardPieceKind, string> = {
    task: 'circle',
    issue: 'issue',
    spec: 'spec',
  };

  // Feed dots reuse the duty feed's colours (F7, F16).
  const eventClasses: Record<GoalEventKind, string> = {
    you: 'you',
    claim: 'claim',
    refused: 'changes',
    opened: 'file',
    changes: 'changes',
    wait: 'wait',
    merged: 'merge',
    left: 'left',
    failed: 'changes',
    finished: 'merge',
  };

  function tone(value: NickTone | null): string {
    if (value === null) return '';
    return typeof value === 'string' ? `nick-${value}` : `nick-${value.Hue}`;
  }

  // The board joins the work pool (GitHub issues, SPEC.md) with saved claims, so it is
  // read again whenever a claim, PR or merge moves the swarm. Every layout snapshot brings a
  // new `swarm` object, many a second while agents stream, so the effect tracks these two
  // values: a derived string only wakes the effect when it actually changes.
  const swarmIdentifier = $derived(swarm.identifier);
  const boardRevision = $derived(swarm.board_revision);
  $effect(() => {
    const identifier = swarmIdentifier;
    void boardRevision;
    const request = ++boardRequest;
    goalSwarmCommands
      .board(identifier)
      .then((next) => {
        if (request !== boardRequest) return;
        board = next;
        boardError = '';
      })
      .catch((reason: unknown) => {
        if (request !== boardRequest) return;
        boardError = reason instanceof Error ? reason.message : String(reason);
      });
  });
</script>

<section
  class="thread-view duty-view swarm-view goal-view"
  aria-label={swarm.title}
>
  <header class="thread-head">
    <h1>{swarm.title}</h1>
    <div class="meta" aria-label="Goal swarm metadata">
      <span>{swarm.agents_label}</span>
      <span>{swarm.started_label}</span>
      {#if swarm.merged_label}<span class="add">{swarm.merged_label}</span>{/if}
    </div>
    <span class="spacer"></span>
    <div class="head-actions">
      <button
        class="btn"
        type="button"
        title="The agents' chat"
        onclick={() => send({ Request: { Navigation: 'Airc' } })}
      >
        AIRC <kbd>A</kbd>
      </button>
      <button
        class="btn"
        type="button"
        title="Add agents on the same prompt"
        onclick={() => send({ AddGoalAgents: swarm.identifier })}
      >
        <Icon name="plus" /> Agents
      </button>
    </div>
  </header>

  <div class="transcript" aria-live="polite">
    <div class="transcript-inner">
      <div class="msg-user">
        {swarm.prompt}
        <div class="sw-from">{swarm.prompt_label}</div>
      </div>

      <div class="sw-sum">
        <div class="sw-big" aria-label="Goal swarm progress">
          {#each swarm.segments as stage, index (index)}<i class="sg {stage}"
            ></i>{/each}
        </div>
        <div class="sw-legend">
          {#each swarm.legend as item (item.stage)}
            <span><i class="sg {item.stage}"></i>{item.label}</span>
          {/each}
          {#if swarm.waiting_label}<span class="waits"
              >{swarm.waiting_label}</span
            >{/if}
        </div>
      </div>

      <div>
        <div class="section-h">
          {board?.heading ?? 'Claims · claims are locked, the first one wins'}
        </div>
        {#if boardError}<p class="board-note error" role="alert">
            {boardError}
          </p>{:else if !board}<p class="board-note">
            Reading the work pool…
          </p>{:else}
          <div class="gb">
            {#each board.rows as row, index (`${row.reference}-${row.title}-${index}`)}
              <div class="gb-r {row.state}">
                <Icon
                  name={row.state === 'merged' ? 'check' : pieceIcons[row.kind]}
                />
                <span class="ttl" title={row.title}>{row.title}</span>
                <span class="ref">{row.reference}</span>
                <span class="who {tone(row.holder_tone)}">{row.holder}</span>
                <span class="st {row.state}" title={row.status}
                  >{row.status}</span
                >
              </div>
            {/each}
          </div>
        {/if}
      </div>

      <div>
        <div class="section-h">
          Agents · same prompt, each picked its own work
        </div>
        <div class="sw-grid">
          {#each swarm.cards as card (card.identifier)}
            <button
              type="button"
              class="sw-card gc {card.stage}"
              onclick={() => send({ OpenThread: card.identifier })}
            >
              <span class="gc-n">
                <b class={tone(card.nick_tone)}>{card.nick}</b>
                <small
                  ><Icon
                    name={providerIcons[card.provider]}
                  />{card.model_label}</small
                >
              </span>
              <span class="ttl" title={card.title}>{card.title}</span>
              <span class="sc-track" title="claim · PR · CI · review · merged">
                {#each card.track as step, index (index)}{#if index > 0}<b
                    ></b>{/if}<i class={step}></i>{/each}
              </span>
              <span class="sc-doing {card.stage}" class:waiting={card.waiting}>
                {#if card.stage === 'working' || card.stage === 'choosing'}<span
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
      id="goal-swarm-composer"
      placeholder={swarm.composer_placeholder}
      modelPill={layout.default_model_pill}
    />
  </div>
</section>
