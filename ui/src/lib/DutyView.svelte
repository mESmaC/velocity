<script lang="ts">
  import { tick } from 'svelte';
  import Composer from './Composer.svelte';
  import Icon from './Icon.svelte';
  import TranscriptBlocks from './TranscriptBlocks.svelte';
  import { dutyCommands } from './ipc';
  import type {
    TranscriptBlock,
    DutyEventKind,
    DutyRole,
    DutyViewSnapshot,
    LayoutAction,
    LayoutSnapshot,
    NavigationIntent,
    ProviderColor,
  } from './ipc';

  let {
    layout,
    duty,
    send,
    onResult,
    onError,
  }: {
    layout: LayoutSnapshot;
    duty: DutyViewSnapshot;
    send: (action: LayoutAction) => void;
    onResult: (snapshot: LayoutSnapshot) => void;
    onError: (reason: unknown) => void;
  } = $props();

  let transcriptEl: HTMLElement | null = null;
  let busy = $state(false);

  // The PR duty agent whose review session is open, and what that session shows. Reviews
  // stream outside the layout snapshot, so the open session is read again while it shows.
  const SESSION_REFRESH_MS = 1500;
  let sessionNickname = $state<string | null>(null);
  let sessionBlocks = $state<TranscriptBlock[] | null>(null);

  $effect(() => {
    const nickname = sessionNickname;
    if (nickname === null || duty.role !== 'pr_duty') return;
    let stopped = false;
    const load = () =>
      dutyCommands
        .agentTranscript(nickname)
        .then((blocks) => {
          if (!stopped) sessionBlocks = blocks;
        })
        .catch(onError);
    void load();
    const timer = globalThis.setInterval(() => void load(), SESSION_REFRESH_MS);
    return () => {
      stopped = true;
      globalThis.clearInterval(timer);
    };
  });

  function toggleSession(nickname: string) {
    sessionBlocks = null;
    sessionNickname = sessionNickname === nickname ? null : nickname;
  }

  const providerIcons: Record<ProviderColor, string> = {
    Claude: 'claude',
    Codex: 'codex',
    Antigravity: 'antigravity',
    Kimi: 'kimi',
    Custom: 'agents',
  };

  const eventClasses: Record<DutyEventKind, string> = {
    you: 'you',
    approve: 'approve',
    merge: 'merge',
    changes: 'changes',
    wait: 'wait',
    file: 'file',
    sweep: 'sweep',
    claim: 'claim',
    skip: 'wait',
    task: 'claim',
  };

  $effect(() => {
    void duty.feed.length;
    void tick().then(() => {
      if (transcriptEl) transcriptEl.scrollTop = transcriptEl.scrollHeight;
    });
  });

  async function togglePause() {
    if (busy) return;
    busy = true;
    try {
      const next = await dutyCommands.pause({ role: duty.role });
      onResult(next);
    } catch (reason) {
      onError(reason);
    } finally {
      busy = false;
    }
  }

  // Dismiss and Stop act at once: SPEC.md §1 rules out confirmation prompts.
  async function run(action: () => Promise<LayoutSnapshot>) {
    if (busy) return;
    busy = true;
    try {
      onResult(await action());
    } catch (reason) {
      onError(reason);
    } finally {
      busy = false;
    }
  }

  function dismiss(nickname: string) {
    void run(() => dutyCommands.dismissAgent({ role: duty.role, nickname }));
  }

  function stopRole() {
    void run(() => dutyCommands.stop({ role: duty.role }));
  }

  const dialogIntents: Record<DutyRole, NavigationIntent> = {
    pr_duty: 'PrDuty',
    scouts: 'Scouts',
    main_watcher: 'MainWatcher',
  };

  function openDialog() {
    send({ Request: { Navigation: dialogIntents[duty.role] } });
  }
</script>

{#snippet doing(agentBusy: boolean, step: string)}
  {#if duty.paused}
    paused
  {:else}
    {#if agentBusy}<span class="working-dots" aria-hidden="true"
        ><span></span><span></span><span></span></span
      >{/if}
    <span class="doing-text" title={step}>{step}</span>
  {/if}
{/snippet}

<section class="thread-view duty-view" aria-label={duty.title}>
  <header class="thread-head">
    <h1>{duty.title}</h1>
    <div class="meta" aria-label="Duty metadata">
      <span>{duty.agent_count_label}</span>
      {#if duty.role === 'pr_duty'}
        <span>{duty.open_pull_request_count} open PRs</span>
        <span class="add">{duty.merged_today} merged today</span>
      {:else if duty.role === 'main_watcher'}
        <span>after every merge</span>
        <span>{duty.issues_filed} flaky tests filed</span>
      {:else}
        <span>{duty.issues_filed} issues filed</span>
        <span>{duty.cadence_label}</span>
      {/if}
      {#if duty.paused}
        <span class="duty-meta-paused">paused</span>
      {/if}
    </div>
    <span class="spacer"></span>
    <div class="head-actions">
      <button class="btn" type="button" disabled={busy} onclick={togglePause}>
        {duty.paused ? 'Resume' : 'Pause'}
      </button>
      <button class="btn" type="button" disabled={busy} onclick={stopRole}
        >Stop</button
      >
      <button class="btn" type="button" onclick={openDialog}>
        <Icon name="plus" /> Agent
      </button>
      <button class="btn" type="button" onclick={openDialog}>Rules</button>
    </div>
  </header>

  <div class="transcript" bind:this={transcriptEl} aria-live="polite">
    <div class="transcript-inner">
      <div>
        <div class="section-h">Rules</div>
        <div class="rule-chips">
          {#each duty.rules as rule (rule.text)}
            <span class="rule" class:hard={rule.hard}>
              {#if rule.hard}<Icon name="lock" />{:else}✓{/if}
              {rule.text}
            </span>
          {/each}
        </div>
      </div>

      <div>
        <div class="section-h">Agents</div>
        <div class="duty-agents">
          {#each duty.agents as agent (agent.nickname)}
            <div
              class="agent-card"
              class:open={agent.nickname === sessionNickname}
            >
              <div class="ah">
                <span class="prov" data-provider={agent.provider}>
                  <Icon name={providerIcons[agent.provider]} />
                </span>
                <b>{agent.nickname}</b>
                <span class="model-name">{agent.model_name}</span>
                <button
                  class="dismiss"
                  type="button"
                  disabled={busy}
                  aria-label="Dismiss {agent.nickname}"
                  title="Dismiss {agent.nickname}"
                  onclick={() => dismiss(agent.nickname)}>×</button
                >
              </div>
              {#if duty.role === 'pr_duty'}
                <button
                  type="button"
                  class="doing open-session"
                  aria-pressed={agent.nickname === sessionNickname}
                  title="Show {agent.nickname}'s review session"
                  onclick={() => toggleSession(agent.nickname)}
                >
                  {@render doing(agent.busy, agent.doing)}
                </button>
              {:else}
                <div class="doing">
                  {@render doing(agent.busy, agent.doing)}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      </div>

      {#if sessionNickname !== null}
        <div class="duty-session">
          <div class="section-h session-h">
            <span>Session · {sessionNickname}</span>
            <button
              type="button"
              class="btn"
              onclick={() => (sessionNickname = null)}>Close</button
            >
          </div>
          {#if sessionBlocks === null}
            <p class="empty">No review since Velocity started.</p>
          {:else}
            <TranscriptBlocks blocks={sessionBlocks} />
          {/if}
        </div>
      {/if}

      <div>
        <div class="section-h">Activity</div>
        <div class="feed">
          {#each duty.feed as event, index (`${event.timestamp}-${event.actor}-${index}`)}
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
      id="duty-composer"
      placeholder={duty.composer_placeholder}
      modelPill={duty.model_pill}
    />
  </div>
</section>
