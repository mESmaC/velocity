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

  $effect(() => {
    const selection = period;
    const projectIdentifier = project;
    const threadIdentifier = thread;
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
