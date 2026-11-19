<script lang="ts">
  import {
    workPoolCommands,
    type LayoutAction,
    type WorkChange,
    type WorkItem,
  } from './ipc';

  let {
    project,
    item,
    owner,
    send,
  }: {
    project: number;
    item?: WorkItem;
    owner: string | null;
    send: (action: LayoutAction) => void;
  } = $props();

  const thread = $derived(item?.thread_identifier?.toString() ?? owner);
  const change = $derived(item?.change);
  let opened = $state<string>();
  let details = $state<WorkChange>();
  let error = $state('');
  let request = 0;

  async function toggle(identifier: string) {
    const current = ++request;
    if (opened === identifier) {
      opened = undefined;
      return;
    }
    opened = identifier;
    details = undefined;
    error = '';
    try {
      const result = await workPoolCommands.change(project, identifier);
      if (request === current) details = result;
    } catch (reason) {
      if (request === current) error = String(reason);
    }
  }
</script>

{#if thread || change}
  <div class="task-sources work-links">
    {#if thread}
      <button
        class="task-source"
        type="button"
        onclick={() => send({ OpenThread: thread })}
      >
        Thread {thread}
      </button>
    {/if}
    {#if change}
      <button
        class="task-source"
        type="button"
        title={change}
        aria-expanded={opened === change}
        onclick={() => toggle(change)}>Change {change.slice(0, 10)}</button
      >
    {/if}
  </div>
{/if}
{#if change && opened === change}
  <div class="change-details" aria-live="polite">
    <span>{change}</span>
    {#if details}
      <span>Owner · {details.owner}</span>
      <span title={details.head}>Head · {details.head.slice(0, 12)}</span>
    {:else if error}
      <span class="issue-error">{error}</span>
    {:else}
      <span>Loading change…</span>
    {/if}
  </div>
{/if}
{#if item?.status === 'reopened' && item.revert_reason}
  <p class="issue-summary">{item.revert_reason}</p>
{/if}

<style>
  .work-links {
    flex-wrap: wrap;
  }
  .change-details {
    display: grid;
    gap: var(--layout-item-gap);
    padding: var(--layout-item-padding);
    color: var(--text-2);
    overflow-wrap: anywhere;
  }
</style>
