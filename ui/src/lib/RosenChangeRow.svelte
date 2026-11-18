<script lang="ts">
  import type { ChangeRow, OwningThread } from './ipc';

  let {
    change,
    owner,
    claims,
    open,
  }: {
    change: ChangeRow;
    owner: OwningThread | undefined;
    claims: number;
    open: (identifier: string) => void;
  } = $props();

  const phases = {
    editing: 'Editing',
    named: 'Named',
    landing: 'Landing',
    landed: 'Landed',
    green: 'Green',
    reverted: 'Reverted · reopened',
    abandoned: 'Abandoned',
  };
</script>

{#snippet details()}
  <strong>{owner?.title ?? change.owner}</strong>
  <span class="mono" title={change.id}
    >{change.id.slice(0, 10)} · {phases[change.phase]}</span
  >
  <span>
    {#if typeof change.added === 'number' && typeof change.removed === 'number'}
      <span class="add">+{change.added}</span>
      <span class="del">−{change.removed}</span> ·
    {/if}
    {claims} path {claims === 1 ? 'claim' : 'claims'}
    {#if change.overlaps.length}
      · {change.overlaps.length} overlapping{/if}
  </span>
  {#if change.parent}
    <span title={change.parent}>Stacked on {change.parent.slice(0, 10)}</span>
  {/if}
  {#if change.landed}
    <span title={change.landed}>Landed at {change.landed.slice(0, 10)}</span>
  {/if}
  {#each change.conflicts as path (path)}
    <span>Conflict · {path}</span>
  {/each}
{/snippet}

{#if owner}
  <button class="change" onclick={() => open(owner.identifier)}>
    {@render details()}
  </button>
{:else}
  <div class="change">{@render details()}</div>
{/if}

{#if change.output}
  <details>
    <summary>Post-land check output</summary>
    <pre>{change.output}</pre>
  </details>
{/if}

<style>
  .change {
    display: flex;
    flex-direction: column;
    gap: var(--rosen-row-gap);
    text-align: left;
    width: 100%;
    padding: var(--rosen-row-padding);
    border-radius: var(--rosen-row-radius);
    border: var(--rosen-section-border);
    background: var(--surface-2);
    color: var(--text);
    margin: var(--rosen-line-margin);
    font-size: var(--rosen-line-size);
    overflow-wrap: anywhere;
  }
  button.change:hover {
    background: var(--surface-3);
  }
  .mono,
  pre {
    font-family: var(--mono);
  }
  details {
    color: var(--text-2);
    font-size: var(--rosen-line-size);
  }
  pre {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
