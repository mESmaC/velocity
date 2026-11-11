<script lang="ts">
  import type { ProofCard } from './ipc';
  let { card }: { card: ProofCard } = $props();

  const marks = { Passed: '✓', Failed: '✗', Pending: '·' } as const;
  const levelLabels = {
    Code: 'code',
    Reference: 'reference',
    Model: 'model',
  } as const;
  let failures = $derived(
    card.checks.filter((check) => check.state === 'Failed' && check.detail),
  );
</script>

<div class="proof" class:failed={!card.proved}>
  <div class="ph">
    <span class="q" aria-hidden="true">∎</span>
    <span
      >Proofs · <code>{card.folders.join(', ')}</code> · checked locally by
      Velocity at <code>{card.head.slice(0, 7)}</code></span
    >
    <span class="st"
      >{card.proved
        ? `${card.theorem_count} theorem${card.theorem_count === 1 ? '' : 's'} proved`
        : 'proof checks failed'}</span
    >
  </div>
  <div class="chk">
    {#each card.checks as check (check.label)}
      <span class={check.state} title={check.detail}
        ><span class="mark">{marks[check.state]}</span> {check.label}</span
      >
    {/each}
  </div>
  {#each failures as check (check.label)}
    <pre class="failure">{check.label}: {check.detail}</pre>
  {/each}
  {#if card.theorems.length}
    <div class="thm">
      {#each card.theorems as theorem, index (index)}
        <span class="k">{theorem.kind}</span><code
          ><b>{theorem.name}</b>{theorem.kind === 'Fixture'
            ? ' · '
            : ' : '}{theorem.statement}</code
        >
      {/each}
    </div>
  {/if}
  {#if card.levels.length}
    <div class="lv">
      {#each card.levels as level (level.language)}
        <span class="k">{level.language}</span><span
          class={level.level}
          class:flagged={level.flagged}
          >{levelLabels[level.level]} · {level.detail}</span
        >
      {/each}
    </div>
  {/if}
</div>
