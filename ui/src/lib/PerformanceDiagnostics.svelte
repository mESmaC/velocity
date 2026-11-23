<script lang="ts">
  import {
    getPerformanceDiagnostics,
    type BudgetCategory,
    type PerformanceDiagnostics,
  } from './ipc';

  let report = $state<PerformanceDiagnostics | null>(null);
  let error = $state('');

  const categoryLabels: Record<BudgetCategory, string> = {
    UiCommand: 'UI command',
    StoreLock: 'Store lock',
    StateSave: 'State save',
    GhRate: 'GitHub rate',
  };

  $effect(() => {
    let active = true;
    let timeout: ReturnType<typeof setTimeout>;
    async function refresh() {
      try {
        const result = await getPerformanceDiagnostics();
        if (!active) return;
        report = result;
        error = '';
      } catch (failure) {
        if (active) error = String(failure);
      } finally {
        if (active) timeout = setTimeout(() => void refresh(), 2000);
      }
    }
    void refresh();
    return () => {
      active = false;
      clearTimeout(timeout);
    };
  });

  const worstCommandP95 = $derived(
    report?.ui_commands.reduce(
      (worst, row) => Math.max(worst, row.p95_ms),
      0,
    ) ?? 0,
  );
  const totalCommandCalls = $derived(
    report?.ui_commands.reduce((sum, row) => sum + row.calls, 0) ?? 0,
  );
  const commandsBreaching = $derived(
    report?.ui_commands.some((row) => row.breaching) ?? false,
  );

  const worstLockHold = $derived(
    report?.store_locks.reduce(
      (worst, row) => Math.max(worst, row.max_hold_ms),
      0,
    ) ?? 0,
  );
  const worstLockWait = $derived(
    report?.store_locks.reduce(
      (worst, row) => Math.max(worst, row.max_wait_ms),
      0,
    ) ?? 0,
  );
  const locksBreaching = $derived(
    report?.store_locks.some((row) => row.breaching) ?? false,
  );

  const GLOBAL_GH_PROJECT = '(global)';

  const peakGhRate = $derived(
    report?.gh_projects.reduce(
      (worst, row) =>
        row.project === GLOBAL_GH_PROJECT
          ? worst
          : Math.max(worst, row.calls_last_minute),
      0,
    ) ?? 0,
  );
  const ghBreaching = $derived(
    report?.gh_projects.some((row) => row.breaching) ?? false,
  );
</script>

<section aria-label="Performance diagnostics">
  <div class="usage-graph-head">
    <h3 class="usage-title">Performance budgets</h3>
    {#if report}
      <span
        class="perf-badge"
        data-state={report.total_breaches > 0 ? 'breach' : 'ok'}
      >
        {#if report.total_breaches > 0}
          {report.total_breaches} budget {report.total_breaches === 1
            ? 'breach'
            : 'breaches'}
        {:else}
          Within budget
        {/if}
      </span>
    {/if}
  </div>

  {#if error}
    <p class="usage-note" role="status">{error}</p>
  {:else if !report}
    <p class="usage-note">Reading performance diagnostics…</p>
  {:else}
    <table class="usage-compare perf-summary" aria-label="Performance budgets">
      <thead>
        <tr>
          <th scope="col">Metric</th>
          <th scope="col">Observed</th>
          <th scope="col">Budget</th>
          <th scope="col">Status</th>
        </tr>
      </thead>
      <tbody>
        <tr>
          <th scope="row">UI commands (p95)</th>
          <td>{worstCommandP95} ms ({totalCommandCalls} calls)</td>
          <td>&lt; {report.budgets.ui_command_p95_ms} ms</td>
          <td
            class="usage-state"
            data-state={commandsBreaching ? 'Failed' : 'Ready'}
          >
            {commandsBreaching ? 'Breach' : 'OK'}
          </td>
        </tr>
        <tr>
          <th scope="row">Store lock hold</th>
          <td>{worstLockHold} ms (wait {worstLockWait} ms)</td>
          <td>&lt; {report.budgets.store_lock_hold_ms} ms</td>
          <td
            class="usage-state"
            data-state={locksBreaching ? 'Failed' : 'Ready'}
          >
            {locksBreaching ? 'Breach' : 'OK'}
          </td>
        </tr>
        <tr>
          <th scope="row">State save lock</th>
          <td>
            {report.saves.max_lock_ms} ms ({report.saves.max_duration_ms} ms total,
            {report.saves.count} saves)
          </td>
          <td>&lt; {report.budgets.save_lock_ms} ms</td>
          <td
            class="usage-state"
            data-state={report.saves.breaching ? 'Failed' : 'Ready'}
          >
            {report.saves.breaching ? 'Breach' : 'OK'}
          </td>
        </tr>
        <tr>
          <th scope="row">GitHub calls / project</th>
          <td>
            {peakGhRate}/min{#if !report.steady_state}
              (startup){/if}
          </td>
          <td>≤ {report.budgets.gh_calls_per_minute}/min</td>
          <td class="usage-state" data-state={ghBreaching ? 'Failed' : 'Ready'}>
            {ghBreaching ? 'Breach' : report.steady_state ? 'OK' : 'Startup'}
          </td>
        </tr>
      </tbody>
    </table>

    {#if report.recent_breaches.length > 0}
      <details class="usage-graph-details" open>
        <summary
          >Recent budget breaches ({report.recent_breaches.length})</summary
        >
        <table
          class="usage-compare perf-table"
          aria-label="Recent budget breaches"
        >
          <thead>
            <tr>
              <th scope="col">Source</th>
              <th scope="col">Observed</th>
              <th scope="col">Budget</th>
            </tr>
          </thead>
          <tbody>
            {#each report.recent_breaches.slice(0, 10) as breach, index (`${breach.unix_ms}-${index}`)}
              <tr title={breach.detail}>
                <th scope="row"
                  >{categoryLabels[breach.category]} · {breach.subject}</th
                >
                <td class="usage-state" data-state="Failed">
                  {breach.observed}
                  {breach.unit}
                </td>
                <td>{breach.budget} {breach.unit}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </details>
    {/if}

    {#if report.ui_commands.length > 0}
      <details class="usage-graph-details" open={commandsBreaching}>
        <summary>UI commands ({report.ui_commands.length})</summary>
        <table class="usage-compare perf-table" aria-label="UI command latency">
          <thead>
            <tr>
              <th scope="col">Command</th>
              <th scope="col">Calls</th>
              <th scope="col">p95</th>
              <th scope="col">Max</th>
            </tr>
          </thead>
          <tbody>
            {#each report.ui_commands.slice(0, 12) as row (row.command)}
              <tr>
                <th scope="row" title={row.command}>{row.command}</th>
                <td>{row.calls}</td>
                <td
                  class="usage-state"
                  data-state={row.breaching ? 'Failed' : undefined}
                >
                  {row.p95_ms} ms
                </td>
                <td>{row.max_ms} ms</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </details>
    {/if}

    {#if report.store_locks.length > 0}
      <details class="usage-graph-details" open={locksBreaching}>
        <summary>Store lock callers ({report.store_locks.length})</summary>
        <table class="usage-compare perf-table" aria-label="Store lock callers">
          <thead>
            <tr>
              <th scope="col">Caller</th>
              <th scope="col">Locks</th>
              <th scope="col">Max wait</th>
              <th scope="col">Max hold</th>
            </tr>
          </thead>
          <tbody>
            {#each report.store_locks.slice(0, 12) as row (row.caller)}
              <tr>
                <th scope="row" title={row.caller}>{row.caller}</th>
                <td>{row.acquisitions}</td>
                <td>{row.max_wait_ms} ms</td>
                <td
                  class="usage-state"
                  data-state={row.breaching ? 'Failed' : undefined}
                >
                  {row.max_hold_ms} ms
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </details>
    {/if}

    {#if report.gh_projects.length > 0}
      <details class="usage-graph-details" open={ghBreaching}>
        <summary>GitHub calls per project ({report.gh_projects.length})</summary
        >
        <table
          class="usage-compare perf-table"
          aria-label="GitHub calls per project"
        >
          <thead>
            <tr>
              <th scope="col">Project</th>
              <th scope="col">Last min</th>
              <th scope="col">Peak/min</th>
              <th scope="col">Total</th>
            </tr>
          </thead>
          <tbody>
            {#each report.gh_projects as row (row.project)}
              <tr>
                <th scope="row" title={row.project}>{row.project}</th>
                <td
                  class="usage-state"
                  data-state={row.breaching ? 'Failed' : undefined}
                >
                  {row.calls_last_minute}
                </td>
                <td>{row.peak_per_minute}</td>
                <td>{row.total_calls}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </details>
    {/if}
  {/if}
</section>

<style>
  h3 {
    margin: 0;
  }
</style>
