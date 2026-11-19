<script lang="ts">
  import type { ChangeRow, OwningThread } from './ipc';

  let {
    change,
    owner,
    claims,
    open,
    openOnCollapse,
  }: {
    change: ChangeRow;
    owner: OwningThread | undefined;
    claims: number;
    open: (identifier: string) => void;
    // Null when the project has no Collapse mirror (#429).
    openOnCollapse: ((commit: string) => void) | null;
  } = $props();

  // A reverted change's commit is no longer in the base, so only standing lands link out.
  const landedCommit = $derived(
    change.phase === 'landed' || change.phase === 'green'
      ? change.landed
      : null,
  );

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

{#if openOnCollapse && landedCommit}
  {@const commit = landedCommit}
  {@const openPage = openOnCollapse}
  <button
    type="button"
    class="btn collapse"
    title="Open this land's commit page on collapse.sh"
    onclick={() => openPage(commit)}>Open on Collapse ↗</button
  >
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
    gap: var(--sp-1);
    text-align: left;
    width: 100%;
    margin: var(--sp-2) 0 0;
    padding: var(--sp-4) var(--sp-5);
    border-radius: var(--r-md);
    border: 1px solid var(--line-dim);
    background: var(--card);
    color: var(--text-2);
    font-size: var(--fs-sm);
    white-space: normal;
    overflow-wrap: anywhere;
  }
  .change strong {
    color: var(--text);
    font-size: var(--fs-md);
    font-weight: var(--fw-semibold);
  }
  .change .mono {
    color: var(--muted);
    font-size: var(--fs-xs);
  }
  button.change:hover {
    border-color: var(--line-mid);
    background: var(--surface-2);
  }
  .mono,
  pre {
    font-family: var(--mono);
  }
  .collapse {
    margin: var(--sp-2) 0 0 var(--sp-2);
  }
  details {
    margin: var(--sp-2) 0 0 var(--sp-2);
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  summary {
    cursor: pointer;
    color: var(--muted);
  }
  pre {
    margin: var(--sp-2) 0 0;
    padding: var(--sp-4) var(--sp-5);
    border: 1px solid var(--line-dim);
    border-radius: var(--r-sm);
    background: rgba(0, 0, 0, 0.42);
    font-size: var(--fs-xs);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
