<script lang="ts">
  import PaneState from './PaneState.svelte';
  import ChangeHistory from './ChangeHistory.svelte';
  import ChangeTrack from './ChangeTrack.svelte';
  import Composer from './Composer.svelte';
  import Icon from './Icon.svelte';
  import Markdown from './Markdown.svelte';
  import TokenUsage from './TokenUsage.svelte';
  import ThreadBrowser from './ThreadBrowser.svelte';
  import TranscriptBlocks from './TranscriptBlocks.svelte';
  import { followTranscript } from './follow-transcript';
  import { threadChangeCommands, threadMenuCommands } from './ipc';
  import type {
    LayoutAction,
    LayoutSnapshot,
    RosenChangeView,
    RosenThreadAction,
    ThreadDetailSnapshot,
  } from './ipc';
  let {
    layout,
    thread,
    browserRequested,
    send,
  }: {
    layout: LayoutSnapshot;
    thread: ThreadDetailSnapshot;
    browserRequested: boolean;
    send: (action: LayoutAction) => void;
  } = $props();
  let usageOpen = $state(false);
  let browserOpen = $state(false);
  $effect(() => {
    const identifier = thread.identifier;
    browserOpen = Boolean(identifier && browserRequested);
  });
  let change = $derived(thread.rosen_change);
  let changeKey = $derived(
    `${layout.project_identifier}:${thread.identifier}:${change?.identifier}`,
  );
  let actions = $state<{
    key: string;
    panel: 'name' | 'abandon' | null;
    pending: boolean;
    error: string;
  }>({
    key: '',
    panel: null,
    pending: false,
    error: '',
  });
  let name = $state('');
  let panel = $derived(actions.key === changeKey ? actions.panel : null);
  let pending = $derived(actions.key === changeKey && actions.pending);
  let actionable = $derived(
    change && !['Landed', 'Green', 'Abandoned'].includes(change.stage),
  );

  function openActions(next: 'name' | 'abandon' | null) {
    name = '';
    actions = { key: changeKey, panel: next, pending: false, error: '' };
  }

  async function runChange(action: RosenThreadAction) {
    const project = layout.project_identifier;
    if (!change || project === null || pending) return;
    const key = changeKey;
    const currentPanel = panel;
    actions = { key, panel: currentPanel, pending: true, error: '' };
    try {
      await threadChangeCommands.run(
        project,
        Number(thread.identifier),
        change.identifier,
        action,
      );
      if (actions.key !== key) return;
      actions = { key, panel: null, pending: false, error: '' };
    } catch (error) {
      if (actions.key === key)
        actions = {
          key,
          panel: currentPanel,
          pending: false,
          error: String(error),
        };
    }
  }

  function changeSteps(view: RosenChangeView) {
    const reopened =
      view.stage === 'Reopened' ||
      (view.stage === 'Conflict' && Boolean(view.lands.at(-1)?.revert));
    const labels = ['Editing', 'Named', 'Landing', 'Landed', 'Green'];
    if (reopened) labels.splice(labels.length - 1, 1, 'Reverted', 'Reopened');
    if (view.stage === 'Abandoned')
      labels.splice(view.name ? 2 : 1, labels.length, 'Abandoned');
    let stage: string = view.stage;
    if (view.stage === 'Conflict') {
      if (reopened) stage = 'Reopened';
      else stage = view.name ? 'Landing' : 'Editing';
    }
    const active = labels.indexOf(stage);
    return labels.map((label, index) => ({
      label,
      state:
        label === 'Reverted' || (index === active && view.stage === 'Conflict')
          ? 'fail'
          : index < active ||
              (index === active && ['Green', 'Abandoned'].includes(view.stage))
            ? 'done'
            : index === active
              ? 'run'
              : 'todo',
    }));
  }

  async function retryResponse() {
    const project = layout.project_identifier;
    const identifier = thread.identifier;
    const menu = await threadMenuCommands.get(Number(identifier));
    if (
      !menu ||
      menu.project_identifier !== project ||
      layout.project_identifier !== project ||
      thread.identifier !== identifier
    )
      return;
    const workspace = menu.entries.some(
      (entry) =>
        typeof entry !== 'string' && entry.Item.action === 'RetryWorkspace',
    );
    if (workspace) {
      await threadMenuCommands.run(
        menu.project_identifier,
        Number(identifier),
        'RetryWorkspace',
      );
    } else {
      send({
        Request: {
          SteerThread: {
            identifier,
            prompt:
              'Continue from the last update and retry the failed operation.',
            queued: false,
          },
        },
      });
    }
  }
</script>

<section class="thread-view" aria-label={thread.title}>
  <header class="thread-head">
    <h1>
      <span class="title-text" title={thread.title}>{thread.title}</span>
      {#if thread.titling}
        <span class="ttl-chip">titling · {thread.title_model_name}</span>
      {/if}
    </h1>
    <div class="meta">
      {#if change}
        <span class="branch mono" title={change.identifier}>
          <Icon name="branch" /><span class="label">{change.identifier}</span>
        </span>
      {:else if thread.branch}
        <span class="branch" title={thread.branch}
          ><Icon name="branch" /><span class="label">{thread.branch}</span
          ></span
        >
      {/if}
      {#if thread.worktree_chip === 'Live'}
        <button
          type="button"
          class="wt"
          title={`${thread.worktree_path} · open in Explorer`}
          onclick={() =>
            send({ Request: { OpenWorktreeInExplorer: thread.worktree_path } })}
        >
          <Icon name="folder" /><span class="label"
            >{thread.worktree_short_path}</span
          >
        </button>
      {:else if thread.worktree_chip === 'Deleted'}
        <span class="wt gone" title={thread.worktree_path}>
          <Icon name="folder" />{change
            ? thread.worktree_short_path
            : 'workspace deleted'}
        </span>
      {/if}
      <span class="add">+{thread.added}</span>
      <span class="del">−{thread.deleted}</span>
      {#if thread.elapsed_label}<span>{thread.elapsed_label}</span>{/if}
    </div>
    <div class="head-actions">
      <button
        class="btn ghost usage-button"
        aria-expanded={usageOpen}
        onclick={() => (usageOpen = !usageOpen)}
        ><Icon name="usage" />Tokens</button
      >
      {#if thread.has_diff_action && (!change || thread.worktree_chip === 'Live')}
        <button
          type="button"
          class="btn"
          title="Review diff"
          onclick={() => send({ Request: { Navigation: 'Diff' } })}
        >
          Diff <kbd>D</kbd>
        </button>
      {/if}
      {#if thread.has_editor_action && (!change || thread.worktree_chip === 'Live')}
        <button
          type="button"
          class="btn"
          title="Open workspace in editor"
          onclick={() => send({ Request: { Navigation: 'Editor' } })}
        >
          Editor <kbd>E</kbd>
        </button>
      {/if}
      {#if actionable && thread.worktree_chip === 'Live'}
        <button
          type="button"
          class="btn primary"
          disabled={pending || thread.agent_busy}
          aria-expanded={panel === 'name'}
          onclick={() => openActions('name')}>Name change</button
        >
        <button
          type="button"
          class="btn"
          disabled={pending || thread.agent_busy}
          onclick={() => runChange('Rebase')}>Rebase</button
        >
        <button
          type="button"
          class="btn"
          disabled={pending || thread.agent_busy}
          aria-expanded={panel === 'abandon'}
          onclick={() => openActions('abandon')}>Abandon</button
        >
      {:else if thread.pull_request_number !== null}
        <button
          type="button"
          class="btn primary"
          title="Open on GitHub"
          onclick={() =>
            send({
              Request: { OpenPullRequest: thread.pull_request_number ?? '0' },
            })}
        >
          GitHub PR #{thread.pull_request_number} ↗
        </button>
      {/if}
    </div>
  </header>

  {#if actionable && panel === 'name'}
    <form
      class="change-action"
      onsubmit={(event) => {
        event.preventDefault();
        if (name.trim() && !thread.agent_busy)
          void runChange({ Name: { message: name } });
      }}
    >
      <label for="change-name">Name change</label>
      <input
        id="change-name"
        bind:value={name}
        placeholder="Summarize the change"
        required
        disabled={pending}
      />
      <button
        class="btn primary"
        disabled={pending || !name.trim() || thread.agent_busy}
        >Name change</button
      >
      <button
        type="button"
        class="btn"
        disabled={pending}
        onclick={() => openActions(null)}>Cancel</button
      >
    </form>
  {:else if actionable && panel === 'abandon'}
    <div class="change-action">
      <span>Abandon this change and remove its workspace?</span>
      <button
        type="button"
        class="btn"
        disabled={pending || thread.agent_busy}
        onclick={() => runChange('Abandon')}>Abandon change</button
      >
      <button
        type="button"
        class="btn"
        disabled={pending}
        onclick={() => openActions(null)}>Cancel</button
      >
    </div>
  {/if}
  {#if actions.key === changeKey && actions.error}
    <div class="change-action" role="alert">{actions.error}</div>
  {/if}

  {#if usageOpen && layout.project_identifier !== null}
    <div class="thread-token-usage">
      <TokenUsage
        project={layout.project_identifier}
        thread={Number(thread.identifier)}
      />
    </div>
  {/if}
  {#if change}
    <ChangeTrack steps={changeSteps(change)} />
  {/if}

  {#if layout.project_identifier !== null}
    <div class="browser-tabs" role="group" aria-label="Thread view">
      <button
        class="tab"
        type="button"
        aria-pressed={!browserOpen}
        onclick={() => (browserOpen = false)}>Transcript</button
      >
      <button
        class="tab"
        type="button"
        aria-pressed={browserOpen}
        onclick={() => (browserOpen = true)}>Browser</button
      >
    </div>
  {/if}
  {#if browserOpen && layout.project_identifier !== null}
    {#key `${layout.project_identifier}:${thread.identifier}`}
      <ThreadBrowser
        project={layout.project_identifier}
        thread={Number(thread.identifier)}
      />
    {/key}
  {:else}
    <div class="transcript" use:followTranscript={thread.identifier}>
      <div class="transcript-inner">
        <PaneState
          name="Transcript"
          rosen
          loading={thread.agent_busy &&
            !thread.blocks.some(
              (block) =>
                block.AgentMessage ||
                block.ToolGroup ||
                block.Diff ||
                block.ErrorCard ||
                block.Decision,
            )}
          empty={!thread.blocks.length &&
            !thread.agent_busy &&
            !change?.lands.length &&
            !change?.conflicts.length &&
            !thread.no_change_reason}
          message="No updates yet · Send a message to start."
        />
        {#if change && thread.worked_summary && thread.transcript_mode !== 'FullTranscript'}
          <div class="worked">
            <Icon name="check" />
            <span>
              Task finished · {thread.worked_summary.tool_calls} tool calls · {thread
                .worked_summary.files_touched} files ·
              <span class="add">+{thread.worked_summary.added}</span>
              <span class="del">−{thread.worked_summary.deleted}</span>
              {thread.worked_summary.proved ? ' · proved' : ''}
            </span>
            <button
              type="button"
              class="mini"
              onclick={() => send('ToggleShowWork')}
            >
              {thread.show_work ? 'Hide work' : 'Show work'}
            </button>
          </div>
        {/if}
        {#if thread.transcript_mode !== 'CollapsedWithLandHistory'}
          <TranscriptBlocks
            blocks={thread.blocks}
            retry={thread.agent_busy ? undefined : retryResponse}
            removeQueued={(index) =>
              send({
                Request: {
                  RemoveQueuedMessage: {
                    identifier: thread.identifier,
                    index: String(index),
                  },
                },
              })}
          />
        {/if}
        {#if change}
          <ChangeHistory
            {change}
            resolve={() => runChange('Resolve')}
            resolving={pending}
          />
        {/if}

        {#if thread.no_change_reason}
          <div class="msg-agent">
            <div class="status-line"><strong>Ended with no change</strong></div>
            <Markdown text={thread.no_change_reason} />
          </div>
        {/if}
      </div>
    </div>

    {#if !thread.no_change_reason && (!change || actionable)}
      <div class="dock">
        <Composer
          {layout}
          {send}
          id="thread-composer"
          placeholder={thread.composer_placeholder}
          modelPill={thread.model_pill}
          threadIdentifier={Number(thread.identifier)}
        />
      </div>
    {/if}
  {/if}
</section>

<style>
  /* Underlined tabs: the view under them is the content, not a choice in a form. */
  .browser-tabs {
    display: flex;
    align-items: flex-end;
    flex: 0 0 auto;
    gap: var(--sp-7);
    padding: 0 var(--sp-3);
    border-bottom: 1px solid var(--line-dim);
  }
  .tab {
    position: relative;
    height: 32px;
    color: var(--muted);
    font-size: var(--fs-md);
    font-weight: var(--fw-medium);
  }
  .tab:hover {
    color: var(--text-2);
  }
  .tab[aria-pressed='true'] {
    color: var(--text);
  }
  .tab[aria-pressed='true']::after {
    content: '';
    position: absolute;
    inset: auto 0 -1px;
    height: 2px;
    border-radius: 2px 2px 0 0;
    background: var(--accent);
  }
</style>
