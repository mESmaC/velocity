<script lang="ts">
  import PaneState from './PaneState.svelte';
  import Composer from './Composer.svelte';
  import Icon from './Icon.svelte';
  import Markdown from './Markdown.svelte';
  import TokenUsage from './TokenUsage.svelte';
  import TranscriptBlocks from './TranscriptBlocks.svelte';
  import { followTranscript } from './follow-transcript';
  import { threadMenuCommands } from './ipc';
  import type {
    LayoutAction,
    LayoutSnapshot,
    ThreadDetailSnapshot,
  } from './ipc';
  let {
    layout,
    thread,
    send,
  }: {
    layout: LayoutSnapshot;
    thread: ThreadDetailSnapshot;
    send: (action: LayoutAction) => void;
  } = $props();
  let usageOpen = $state(false);

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
      {#if thread.account_label}<span>{thread.account_label}</span>{/if}
      {#if thread.branch}
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
          <Icon name="folder" />workspace deleted
        </span>
      {/if}
      <span class="add">+{thread.added}</span>
      <span class="del">−{thread.deleted}</span>
      {#if thread.elapsed_label}<span>{thread.elapsed_label}</span>{/if}
    </div>
    <div class="head-actions">
      <button
        class="usage-button"
        aria-expanded={usageOpen}
        onclick={() => (usageOpen = !usageOpen)}>Token usage</button
      >
      {#if thread.has_diff_action}
        <button
          type="button"
          class="btn"
          title="Review diff"
          onclick={() => send({ Request: { Navigation: 'Diff' } })}
        >
          Diff <kbd>D</kbd>
        </button>
      {/if}
      {#if thread.has_editor_action}
        <button
          type="button"
          class="btn"
          title="Open workspace in editor"
          onclick={() => send({ Request: { Navigation: 'Editor' } })}
        >
          Editor <kbd>E</kbd>
        </button>
      {/if}
      {#if thread.pull_request_number !== null}
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

  {#if usageOpen && layout.project_identifier !== null}
    <div class="thread-token-usage">
      <TokenUsage
        project={layout.project_identifier}
        thread={Number(thread.identifier)}
      />
    </div>
  {/if}

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
          !thread.no_change_reason}
        message="No updates yet · Send a message to start."
      />
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
      {#if thread.no_change_reason}
        <div class="msg-agent">
          <div class="status-line"><strong>Ended with no change</strong></div>
          <Markdown text={thread.no_change_reason} />
        </div>
      {/if}
    </div>
  </div>

  {#if !thread.no_change_reason}
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
</section>
