<script lang="ts">
  import Icon from './Icon.svelte';
  import { formatSubagentMeta, formatSubagentStatus } from './subagent-format';
  import type { SubagentView } from './ipc';

  let {
    subagent,
    defaultModel,
    nowSeconds,
    selected = false,
    onOpen,
  }: {
    subagent: SubagentView;
    defaultModel: string;
    nowSeconds: number;
    selected?: boolean;
    onOpen: (identifier: string, opener: HTMLElement) => void;
  } = $props();

  const meta = $derived(formatSubagentMeta(subagent, defaultModel, nowSeconds));
  const statusLabel = $derived(formatSubagentStatus(subagent.status));
  const currentStep = $derived(
    subagent.current_step ??
      (subagent.status === 'Running'
        ? 'Starting…'
        : subagent.status === 'Failed'
          ? 'Failed'
          : 'Finished'),
  );
</script>

<button
  type="button"
  class="subagent-card"
  class:selected
  data-status={subagent.status}
  data-subagent-id={subagent.identifier}
  aria-expanded={selected}
  onclick={(event) => onOpen(subagent.identifier, event.currentTarget)}
>
  <span class="ah">
    {#if subagent.status === 'Running'}
      <span class="working-dots" aria-hidden="true"><i></i><i></i><i></i></span>
    {:else if subagent.status === 'Failed'}
      <span class="subagent-status-icon failed"><Icon name="alert" /></span>
    {:else}
      <span class="subagent-status-icon done"><Icon name="check" /></span>
    {/if}
    <b class="subagent-title" title={subagent.title}>{subagent.title}</b>
    <span class="subagent-status">{statusLabel}</span>
  </span>
  <span class="subagent-meta">{meta}</span>
  <span class="doing" title={currentStep}>{currentStep}</span>
</button>
