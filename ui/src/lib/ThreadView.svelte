<script lang="ts">
  import Composer from './Composer.svelte';
  import Icon from './Icon.svelte';
  import PullRequestHistory from './PullRequestHistory.svelte';
  import TranscriptBlocks from './TranscriptBlocks.svelte';
  import { followTranscript } from './follow-transcript';
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
</script>

<section class="thread-view" aria-label={thread.title}>
  <header class="thread-head">
    <h1>
      <span class="title-text">{thread.title}</span>
      {#if thread.titling}
        <span class="ttl-chip">titling · {thread.title_model_name}</span>
      {/if}
    </h1>
    <div class="meta">
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
          <Icon name="folder" />worktree deleted
        </span>
      {/if}
      <span class="add">+{thread.added}</span>
      <span class="del">−{thread.deleted}</span>
      {#if thread.elapsed_label}<span>{thread.elapsed_label}</span>{/if}
    </div>
    <div class="head-actions">
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
          title="Open worktree in editor"
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
          PR #{thread.pull_request_number} ↗
        </button>
      {/if}
    </div>
  </header>

  {#if thread.pull_request_history}<PullRequestHistory
      history={thread.pull_request_history}
      track
    />{/if}

  <div class="transcript" use:followTranscript={thread.identifier}>
    <div class="transcript-inner">
      {#if thread.worked_summary && thread.transcript_mode !== 'FullTranscript'}
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

      {#if thread.transcript_mode !== 'CollapsedWithPrHistory'}
        <TranscriptBlocks
          blocks={thread.blocks}
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
      {#if thread.pull_request_history}<PullRequestHistory
          history={thread.pull_request_history}
        />{/if}
    </div>
  </div>

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
</section>
