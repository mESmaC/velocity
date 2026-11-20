<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import PaneState from './PaneState.svelte';
  import TranscriptBlocks from './TranscriptBlocks.svelte';
  import { followTranscript } from './follow-transcript';
  import { formatSubagentMeta, formatSubagentStatus } from './subagent-format';
  import type { SubagentView } from './ipc';

  let {
    subagent,
    index,
    total,
    defaultModel,
    nowSeconds,
    subagents,
    onSelect,
    onStep,
    onClose,
  }: {
    subagent: SubagentView;
    index: number;
    total: number;
    defaultModel: string;
    nowSeconds: number;
    subagents: SubagentView[];
    onSelect: (identifier: string, opener: HTMLElement) => void;
    onStep: (delta: -1 | 1) => void;
    onClose: () => void;
  } = $props();

  let closeButton: HTMLElement | undefined = $state();

  const meta = $derived(formatSubagentMeta(subagent, defaultModel, nowSeconds));
  const statusLabel = $derived(formatSubagentStatus(subagent.status));
  const nestedSubagents = $derived(
    subagents.filter((entry) => entry.identifier !== subagent.identifier),
  );

  onMount(() => {
    closeButton?.focus();
  });
</script>

<aside class="subagent-panel" aria-label={`Subagent: ${subagent.title}`}>
  <header class="subagent-panel-head">
    <div class="subagent-panel-heading">
      {#if subagent.status === 'Running'}
        <span class="working-dots" aria-hidden="true"
          ><i></i><i></i><i></i></span
        >
      {:else if subagent.status === 'Failed'}
        <span class="subagent-status-icon failed"><Icon name="alert" /></span>
      {:else}
        <span class="subagent-status-icon done"><Icon name="check" /></span>
      {/if}
      <h2 class="subagent-panel-title" title={subagent.title}>
        {subagent.title}
      </h2>
    </div>
    <div class="subagent-panel-controls">
      {#if total > 1}
        <span class="subagent-panel-count">{index + 1}/{total}</span>
      {/if}
      <button
        type="button"
        class="mini"
        disabled={total <= 1}
        aria-label="Previous subagent"
        title="Previous subagent (Alt+↑)"
        onclick={() => onStep(-1)}>←</button
      >
      <button
        type="button"
        class="mini"
        disabled={total <= 1}
        aria-label="Next subagent"
        title="Next subagent (Alt+↓)"
        onclick={() => onStep(1)}>→</button
      >
      <button
        type="button"
        class="mini"
        bind:this={closeButton}
        aria-label="Close subagent panel"
        title="Close subagent panel (Esc)"
        onclick={onClose}>×</button
      >
    </div>
  </header>

  <div class="subagent-panel-meta" data-status={subagent.status}>
    <span class="subagent-panel-live">
      <span class="subagent-status">{statusLabel}</span>
      <span>· {meta}</span>
    </span>
    <span class="subagent-panel-hint">
      {subagent.status === 'Running'
        ? 'Live transcript · read only'
        : 'Transcript · read only'}
    </span>
  </div>

  <div class="subagent-panel-body" use:followTranscript={subagent.identifier}>
    <div class="transcript-inner">
      {#if subagent.blocks.length === 0}
        <PaneState
          name="Subagent"
          rosen
          loading={subagent.status === 'Running'}
          empty={subagent.status !== 'Running'}
          message="No transcript steps recorded."
        />
      {:else}
        <TranscriptBlocks
          blocks={subagent.blocks}
          subagents={nestedSubagents}
          {defaultModel}
          {nowSeconds}
          openSubagent={onSelect}
          showUnmatched={false}
        />
      {/if}
    </div>
  </div>
</aside>
