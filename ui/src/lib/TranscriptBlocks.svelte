<script lang="ts">
  import PaneState from './PaneState.svelte';
  import Markdown from './Markdown.svelte';
  import SubagentCard from './SubagentCard.svelte';
  import { rise } from './motion';
  import {
    findUnmatchedSubagents,
    partitionToolGroup,
  } from './subagent-format';
  import {
    computeVisibleRange,
    createHeightTracker,
    DEFAULT_GAP_PX,
    observeScrollContainer,
    VIRTUALIZE_THRESHOLD,
  } from './virtual-list';
  import type { DiffLineRole, SubagentView, TranscriptBlock } from './ipc';

  let {
    blocks,
    blockIds,
    transcriptKey = '',
    scrollContainer = null,
    removeQueued,
    retry,
    subagents = [],
    activeSubagentId = null,
    defaultModel = '',
    nowSeconds = 0,
    openSubagent,
    showUnmatched = true,
  }: {
    blocks: TranscriptBlock[];
    /** Stable per-block IDs matching `blocks`; when omitted, fallback IDs are assigned per reset. */
    blockIds?: readonly number[];
    /** Changes when switching to a different transcript so cached item heights reset cleanly. */
    transcriptKey?: string;
    /** Scrollable ancestor viewport used to virtualize long transcripts (#553). */
    scrollContainer?: HTMLElement | null;
    retry?: () => void | Promise<void>;
    /** Withdraws the queued message at this position among the queued rows (F4). */
    removeQueued?: (index: number) => void;
    subagents?: SubagentView[];
    activeSubagentId?: string | null;
    defaultModel?: string;
    nowSeconds?: number;
    openSubagent?: (identifier: string, opener: HTMLElement) => void;
    showUnmatched?: boolean;
  } = $props();

  let listEl: HTMLElement | undefined = $state();
  let scrollTop = $state(0);
  let viewportHeight = $state(800);
  let listOffsetTop = $state(0);
  let heightVersion = $state(0);

  const heightTracker = createHeightTracker(
    () => scrollContainer,
    () => {
      heightVersion += 1;
    },
  );

  let lastTranscriptKey = '';
  $effect(() => {
    if (transcriptKey !== lastTranscriptKey) {
      lastTranscriptKey = transcriptKey;
      heightTracker.clear();
      heightVersion += 1;
    }
  });

  $effect(() => {
    return () => {
      heightTracker.destroy();
    };
  });

  // Fallback stable IDs when a caller passes plain `blocks` without `blockIds`.
  let fallbackEpoch = 0;
  const fallbackIds = $derived.by(() => {
    if (blockIds && blockIds.length === blocks.length) return blockIds;
    const base = fallbackEpoch * 1_000_000;
    return blocks.map((_, index) => base + index + 1);
  });
  $effect(() => {
    void transcriptKey;
    fallbackEpoch += 1;
  });

  // Track scroll position and viewport size of the scroll container at 60 fps.
  $effect(() => {
    const container = scrollContainer;
    if (!container) return;
    return observeScrollContainer(container, () => {
      scrollTop = container.scrollTop;
      viewportHeight = container.clientHeight || 800;
      if (listEl) {
        const containerRect = container.getBoundingClientRect();
        const listRect = listEl.getBoundingClientRect();
        listOffsetTop = Math.max(
          0,
          listRect.top - containerRect.top + container.scrollTop,
        );
      }
    });
  });

  function measureItem(node: HTMLElement, id: number) {
    heightTracker.observe(node, id);
    return {
      update(nextId: number) {
        heightTracker.updateId(node, nextId);
      },
      destroy() {
        heightTracker.unobserve(node);
      },
    };
  }

  const virtualized = $derived(
    scrollContainer !== null && blocks.length > VIRTUALIZE_THRESHOLD,
  );

  const range = $derived.by(() => {
    void heightVersion;
    if (!virtualized) {
      return { start: 0, end: blocks.length, topPad: 0, bottomPad: 0 };
    }
    return computeVisibleRange(
      blocks,
      fallbackIds,
      heightTracker.heights,
      Math.max(0, scrollTop - listOffsetTop),
      viewportHeight,
      DEFAULT_GAP_PX,
    );
  });

  interface VisibleBlock {
    id: number;
    index: number;
    block: TranscriptBlock;
  }

  const visibleBlocks = $derived.by((): VisibleBlock[] => {
    const items: VisibleBlock[] = [];
    for (let index = range.start; index < range.end; index += 1) {
      items.push({
        id: fallbackIds[index],
        index,
        block: blocks[index],
      });
    }
    return items;
  });

  // Queued rows sit at the end in send order; scanning backward from the tail is O(queued).
  const firstQueuedBlockIndex = $derived.by(() => {
    let cursor = blocks.length;
    while (cursor > 0 && blocks[cursor - 1].UserMessage?.queued) {
      cursor -= 1;
    }
    return cursor;
  });

  const subagentById = $derived(
    new Map(subagents.map((subagent) => [subagent.identifier, subagent])),
  );

  // Subagents without a matching parent tool call (for example Codex native subagent
  // sessions, §3.2 Subagents) render in a group at the end of the transcript.
  const unmatchedSubagents = $derived(
    showUnmatched && subagents.length > 0
      ? findUnmatchedSubagents(blocks, subagents)
      : [],
  );

  const diffLineClasses: Record<DiffLineRole, string> = {
    Added: 'a',
    Deleted: 'd',
    Context: '',
    HunkHeader: 'c',
  };

  function agentMarkdownText(paragraphs: string[]): string {
    return paragraphs.length === 1 ? paragraphs[0] : paragraphs.join('\n\n');
  }
</script>

<div
  class="transcript-list"
  bind:this={listEl}
  style:padding-top={virtualized && range.topPad > 0
    ? `${range.topPad}px`
    : undefined}
  style:padding-bottom={virtualized && range.bottomPad > 0
    ? `${range.bottomPad}px`
    : undefined}
>
  {#each visibleBlocks as item (item.id)}
    {@const block = item.block}
    <div
      class="transcript-item"
      data-block-id={item.id}
      use:measureItem={item.id}
    >
      {#if block.UserMessage}
        <div class="msg-user">
          <Markdown text={block.UserMessage.text} />
        </div>
        {#if block.UserMessage.queued}
          <div class="queued">
            queued · sends when the current step finishes{#if removeQueued}<span
              >
                ·
              </span><button
                type="button"
                class="queued-remove"
                title="Remove this message before the agent reads it"
                onclick={() => removeQueued(item.index - firstQueuedBlockIndex)}
                >remove</button
              >{/if}
          </div>
        {/if}
      {:else if block.Notice}
        <div class="msg-user">
          <Markdown text={block.Notice.text} />
          <div class="sw-from">from Velocity · read by the agent</div>
        </div>
      {:else if block.AgentMessage}
        <div class="msg-agent">
          <Markdown text={agentMarkdownText(block.AgentMessage.paragraphs)} />
        </div>
      {:else if block.ToolGroup}
        {#each partitionToolGroup(block.ToolGroup.steps, subagentById) as segment, segmentIndex (segmentIndex)}
          {#if segment.kind === 'tools'}
            <div class="tools">
              {#each segment.steps as step, stepIndex (step.tool_call_id ?? stepIndex)}
                <div class="tool" class:running={step.running}>
                  {#if step.running}
                    <span class="working-dots" aria-hidden="true"
                      ><i></i><i></i><i></i></span
                    >
                  {:else}
                    <span class="ok">✓</span>
                  {/if}
                  <span class="verb" title={step.verb}>{step.verb}</span>
                  <span class="arg" title={step.argument}>{step.argument}</span>
                  <span class="res" title={step.result_text}
                    >{step.result_text}</span
                  >
                </div>
              {/each}
            </div>
          {:else}
            <div class="subagent-section">
              <div class="section-h">
                {segment.subagents.length === 1
                  ? 'Agent · subagent'
                  : `Agent · ${segment.subagents.length} parallel subagents`}
              </div>
              <div class="subagent-group">
                {#each segment.subagents as subagent (subagent.identifier)}
                  <SubagentCard
                    {subagent}
                    {defaultModel}
                    {nowSeconds}
                    selected={activeSubagentId === subagent.identifier}
                    onOpen={(identifier, opener) =>
                      openSubagent?.(identifier, opener)}
                  />
                {/each}
              </div>
            </div>
          {/if}
        {/each}
      {:else if block.Diff}
        <div class="diff">
          <div class="diff-h">
            <span class="diff-path" title={block.Diff.block.file_path}
              >{block.Diff.block.file_path}</span
            >
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
        <PaneState
          name="Agent response"
          error={block.ErrorCard.card.detail || block.ErrorCard.card.headline}
          {retry}
          automatic={false}
        />
      {:else if block.Decision}
        <div class="qa">
          <div class="q">{block.Decision.question}</div>
          <div class="d">
            ✓ <b>{block.Decision.key}</b><span>{block.Decision.value}</span>
          </div>
        </div>
      {/if}
    </div>
  {/each}
</div>
{#if unmatchedSubagents.length > 0}
  <div class="subagent-section" in:rise>
    <div class="section-h">
      {unmatchedSubagents.length === 1
        ? 'Agent · subagent'
        : `Agent · ${unmatchedSubagents.length} subagents`}
    </div>
    <div class="subagent-group">
      {#each unmatchedSubagents as subagent (subagent.identifier)}
        <SubagentCard
          {subagent}
          {defaultModel}
          {nowSeconds}
          selected={activeSubagentId === subagent.identifier}
          onOpen={(identifier, opener) => openSubagent?.(identifier, opener)}
        />
      {/each}
    </div>
  </div>
{/if}
