<script lang="ts">
  import PaneState from './PaneState.svelte';
  import TokenUsageTable from './TokenUsageTable.svelte';
  import { tokenUsageCommands, type Period, type Report } from './ipc';
  import './theme/usage.css';

  let {
    thread = null,
    project = null,
  }: {
    thread?: number | null;
    project?: number | null;
  } = $props();

  let period = $state<Period>('Today');
  let report = $state<Report | null>(null);
  let error = $state('');

  // The thread view passes fields of the layout snapshot, which is replaced on every
  // publish; deriveds keep the report from blanking and refetching each time (#491).
  const currentProject = $derived(project);
  const currentThread = $derived(thread);

  $effect(() => {
    const selection = period;
    const projectIdentifier = currentProject;
    const threadIdentifier = currentThread;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    report = null;
    error = '';

    async function refresh() {
      try {
        const result = await tokenUsageCommands.get(
          selection,
          projectIdentifier,
          threadIdentifier,
        );
        if (!stopped) {
          report = result;
          error = '';
        }
      } catch (failure) {
        if (!stopped) error = String(failure);
      } finally {
        if (!stopped) timer = setTimeout(() => void refresh(), 5000);
      }
    }

    void refresh();
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
  });
</script>

<section aria-label="Token usage">
  <div class="usage-graph-head">
    <div class="usage-title">
      {thread === null ? 'Token usage' : 'Lifetime token usage'}
    </div>
    {#if thread === null}
      <div class="usage-seg" role="group" aria-label="Token usage period">
        <button
          aria-pressed={period === 'Today'}
          class:on={period === 'Today'}
          onclick={() => (period = 'Today')}>Today</button
        >
        <button
          aria-pressed={period === 'Week'}
          class:on={period === 'Week'}
          onclick={() => (period = 'Week')}>Last 7 days</button
        >
      </div>
    {/if}
  </div>
  {#if report?.groups.some((group) => group.rows.length)}
    <p class="usage-note">
      Cached input is included in input. Expand a row for estimated input by
      source.
    </p>
    {#each report.groups as group (group.name)}
      {#if group.rows.length}<TokenUsageTable {group} />{/if}
    {/each}
  {:else}
    <PaneState
      name="Token usage"
      {error}
      loading={!report && !error}
      empty={!!report}
      message="No token usage recorded for this period."
    />
  {/if}
</section>

{#if report?.handoff}
  {@const comparison = report.handoff}
  <section aria-label="Context hand-off trial">
    <div class="usage-title">Context hand-off trial</div>
    <p class="usage-note">
      Lifetime totals for matched Rosen items that reached 120,000 context
      tokens and passed review and build checks.
    </p>
    <table class="usage-compare token-usage-table">
      <thead>
        <tr>
          <th scope="col">Setting</th>
          <th scope="col">Matched items</th>
          <th scope="col">Tokens</th>
          <th scope="col">Review rounds</th>
          <th scope="col">Reverts</th>
        </tr>
      </thead>
      <tbody>
        {#each [{ label: 'On', cohort: comparison.enabled }, { label: 'Off', cohort: comparison.disabled }] as { label, cohort } (label)}
          <tr>
            <th scope="row">{label}</th>
            <td>{cohort.matched} of {cohort.eligible}</td>
            <td>{cohort.tokens}</td>
            <td>{cohort.review_rounds}</td>
            <td>{cohort.reverts}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    <p class="usage-note">
      {comparison.handed_off} matched items continued in a fresh session.
      {#if typeof comparison.savings_percent === 'number'}
        {Math.abs(comparison.savings_percent).toFixed(1)}%
        {comparison.savings_percent >= 0 ? 'fewer' : 'more'} tokens with the setting
        on.
      {/if}
      {#if comparison.result === 'Collecting'}
        Waiting for at least 10 hand-offs and 10 matched controls.
      {:else if comparison.result === 'Met'}
        Trial target met.
      {:else}
        Trial target missed.
      {/if}
    </p>
  </section>
{/if}
