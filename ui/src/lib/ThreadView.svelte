<script lang="ts">
  import Composer from './Composer.svelte';
  import Icon from './Icon.svelte';
  import type {
    DiffLineRole,
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

  const diffLineClasses: Record<DiffLineRole, string> = {
    Added: 'a',
    Deleted: 'd',
    Context: '',
    HunkHeader: 'c',
  };
</script>

<section class="thread-view" aria-label={thread.title}>
  <header class="thread-head">
    <h1>
      {thread.title}
      {#if thread.titling}
        <span class="ttl-chip">titling · {thread.title_model_name}</span>
      {/if}
    </h1>
    <div class="meta">
      {#if thread.branch}
        <span class="branch"><Icon name="branch" />{thread.branch}</span>
      {/if}
      {#if thread.worktree_chip === 'Live'}
        <button
          type="button"
          class="wt"
          title={`${thread.worktree_path} · open in Explorer`}
          onclick={() =>
            send({ Request: { OpenWorktreeInExplorer: thread.worktree_path } })}
        >
          <Icon name="folder" />{thread.worktree_short_path}
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
    <span class="spacer"></span>
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

  <div class="transcript">
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
        {#each thread.blocks as block, blockIndex (blockIndex)}
          {#if block.UserMessage}
            <div class="msg-user">{block.UserMessage.text}</div>
            {#if block.UserMessage.queued}
              <div class="queued">
                queued · sends when the current step finishes
              </div>
            {/if}
          {:else if block.AgentMessage}
            <div class="msg-agent">
              {#each block.AgentMessage.paragraphs as paragraph, paragraphIndex (paragraphIndex)}
                <p>{paragraph}</p>
              {/each}
            </div>
          {:else if block.ToolGroup}
            <div class="tools">
              {#each block.ToolGroup.steps as step, stepIndex (stepIndex)}
                <div class="tool" class:running={step.running}>
                  {#if step.running}
                    <span class="working-dots" aria-hidden="true"
                      ><i></i><i></i><i></i></span
                    >
                  {:else}
                    <span class="ok">✓</span>
                  {/if}
                  <span class="verb">{step.verb}</span>
                  <span class="arg">{step.argument}</span>
                  <span class="res">{step.result_text}</span>
                </div>
              {/each}
            </div>
          {:else if block.Diff}
            <div class="diff">
              <div class="diff-h">
                <span>{block.Diff.block.file_path}</span>
                <span>
                  <span class="add">+{block.Diff.block.added}</span>
                  <span class="del">−{block.Diff.block.deleted}</span>
                </span>
              </div>
              <pre>{#each block.Diff.block.lines as line, lineIndex (lineIndex)}<span
                    class={diffLineClasses[line.role]}>{line.text}</span
                  >{/each}</pre>
            </div>
          {:else if block.StatusLine}
            <div class="status-line">
              {#if block.StatusLine.running}
                <span class="working-dots" aria-hidden="true"
                  ><i></i><i></i><i></i></span
                >
              {/if}
              <span>{block.StatusLine.text}</span>
            </div>
          {:else if block.ErrorCard}
            <div class="err-card" role="alert">
              <div class="ah">
                <Icon name="alert" />{block.ErrorCard.card.headline}
              </div>
              <pre>{block.ErrorCard.card.detail}</pre>
            </div>
          {/if}
        {/each}
      {/if}
    </div>
  </div>

  <div class="dock">
    <Composer
      {layout}
      {send}
      id="thread-composer"
      placeholder={thread.composer_placeholder}
      modelPill={thread.model_pill}
    />
  </div>
</section>
