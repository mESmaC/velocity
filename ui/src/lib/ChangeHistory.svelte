<script lang="ts">
  import type { RosenChangeView } from './ipc';

  let {
    change,
    resolve,
    abandon,
    resolving,
    resolveError = '',
    openOnCollapse,
  }: {
    change: RosenChangeView;
    resolve: () => void;
    abandon?: () => void;
    resolving: boolean;
    resolveError?: string;
    // Null when the project has no Collapse mirror (#429).
    openOnCollapse: ((commit: string) => void) | null;
  } = $props();

  let confirmAbandon = $state(false);
  $effect(() => {
    void change.identifier;
    if (!change.resolution?.offer_abandon) confirmAbandon = false;
  });
</script>

{#if change.conflicts.length}
  <section class="err-card" aria-label="Stored conflicts">
    <div class="ah">Stored conflicts</div>
    <ul class="change-conflicts">
      {#each change.conflicts as path (path)}
        <li class="mono">{path}</li>
      {/each}
    </ul>
    {#if change.resolution}
      <p
        class="change-resolution"
        class:blocked={change.resolution.offer_abandon}
        role={change.resolution.offer_abandon ? 'alert' : 'status'}
      >
        {change.resolution.status}
      </p>
    {/if}
    {#if resolveError}
      <p class="change-resolution blocked" role="alert">{resolveError}</p>
    {/if}
    <div class="change-conflicts-actions">
      <button type="button" class="btn" disabled={resolving} onclick={resolve}>
        {resolving ? 'Resolving…' : 'Resolve with agent'}
      </button>
      {#if change.resolution?.offer_abandon && abandon}
        {@const runAbandon = abandon}
        {#if confirmAbandon}
          <span class="change-resolution">Abandon this change?</span>
          <button
            type="button"
            class="btn"
            disabled={resolving}
            onclick={() => {
              confirmAbandon = false;
              runAbandon();
            }}>Abandon change</button
          >
          <button
            type="button"
            class="btn"
            disabled={resolving}
            onclick={() => (confirmAbandon = false)}>Cancel</button
          >
        {:else}
          <button
            type="button"
            class="btn"
            disabled={resolving}
            onclick={() => (confirmAbandon = true)}>Abandon</button
          >
        {/if}
      {/if}
    </div>
  </section>
{/if}

{#if change.lands.length}
  <section class="change-history" aria-label="Land history">
    <h2 class="section-h">Land history</h2>
    {#each change.lands as land (land.commit)}
      <article class="done-card">
        <div class="ah">Landed · <span class="mono">{land.commit}</span></div>
        <div>{land.summary}</div>
        <dl class="change-trailers mono">
          <dt>Rosen-Change</dt>
          <dd>{land.change}</dd>
          {#if land.receipt}
            <dt>Rosen-Receipt</dt>
            <dd>{land.receipt}</dd>
          {/if}
          {#each land.approvers as approver, index (index)}
            <dt>Rosen-Approved-By</dt>
            <dd>{approver}</dd>
          {/each}
        </dl>
        {#if land.revert}
          <div>Reverted · <span class="mono">{land.revert}</span></div>
        {:else if land.checks_passed}
          <div>Post-land checks passed</div>
        {:else if land.build_passed}
          <div>Build and lint passed</div>
          {#if !land.failures.length}<div>Post-land checks pending</div>{/if}
        {:else if !land.failures.length}
          <div>Post-land checks pending</div>
        {/if}
        {#if openOnCollapse && !land.revert}
          {@const open = openOnCollapse}
          <button
            type="button"
            class="btn"
            title="Open this land's commit page on collapse.sh"
            onclick={() => open(land.commit)}>Open on Collapse ↗</button
          >
        {/if}
      </article>
      {#each land.failures as failure, index (index)}
        <div class="err-card">
          <div class="ah">{failure.label}</div>
          <pre>{failure.output}</pre>
        </div>
      {/each}
    {/each}
  </section>
{:else if change.stage === 'Abandoned'}
  <div class="closed-card">Change abandoned</div>
{/if}
