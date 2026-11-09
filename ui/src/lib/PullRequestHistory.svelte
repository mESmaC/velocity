<script lang="ts">
  import type { PullRequestHistoryView } from './ipc';
  let {
    history,
    track = false,
  }: { history: PullRequestHistoryView; track?: boolean } = $props();
</script>

{#if track}
  <div class="pr-track" aria-label="Pull request progress">
    {#each history.track as step, index (index)}
      {#if index}<span class="bar" class:done={step.state === 'done'}
        ></span>{/if}
      <span class={`step ${step.state}`}
        ><i aria-hidden="true"></i><span>{step.label}</span></span
      >
    {/each}
  </div>
{:else}
  {#each history.cards as card (card.identifier)}
    {#if card.tool}
      <div class="tools">
        <div class="tool" class:running={card.tool.running}>
          <span class="ok">{card.tool.running ? '…' : '✓'}</span>
          <span class="verb">{card.tool.verb}</span><span class="arg"
            >{card.tool.argument}</span
          ><span class="res">{card.tool.result_text}</span>
        </div>
      </div>
    {:else if card.path}
      <div class="rc" class:resolved={card.resolved}>
        <div class="rc-h">
          <span>{card.title}</span>
          <span class="mono"
            >{card.path}{card.line === null ? '' : `:${card.line}`}</span
          >
          <span class="st">{card.resolved ? '✓ resolved' : 'open'}</span>
        </div>
        <div class="rc-b">{card.body}</div>
      </div>
    {:else if card.identifier === 'closed' || card.identifier === 'merged'}
      <div
        class:closed-card={card.identifier === 'closed'}
        class:done-card={card.identifier === 'merged'}
      >
        <div class="ah">{card.title}</div>
        {#if card.identifier === 'closed'}<div class="kept">
            {card.body}
          </div>{/if}
      </div>
    {:else}
      <div class="pl">
        <span class="pi" aria-hidden="true"></span><span
          ><b>{card.title}</b>{#if card.body}<span class="pr-detail"
              >{card.body}</span
            >{/if}</span
        >
      </div>
    {/if}
  {/each}
  {#if history.cleanup_error}
    <div class="err-card" role="alert">
      <div class="ah">Worktree cleanup will retry</div>
      <pre>{history.cleanup_error}</pre>
    </div>
  {/if}
  {#if history.stage !== 'merged' && history.stage !== 'closed'}
    <div class="status-line" role="status">{history.status}</div>
  {/if}
{/if}
