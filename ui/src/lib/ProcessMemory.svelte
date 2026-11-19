<script lang="ts">
  import { getProcessMemory, type ProcessMemory } from './ipc';

  let processes = $state<ProcessMemory[]>([]);
  let error = $state('');

  $effect(() => {
    let active = true;
    let timeout: ReturnType<typeof setTimeout>;
    async function refresh() {
      try {
        const result = await getProcessMemory();
        if (!active) return;
        processes = result;
        error = '';
      } catch (failure) {
        if (active) error = String(failure);
      } finally {
        if (active) timeout = setTimeout(() => void refresh(), 5000);
      }
    }
    void refresh();
    return () => {
      active = false;
      clearTimeout(timeout);
    };
  });
</script>

<section aria-label="Process memory">
  <h3 class="usage-title">Process memory</h3>
  {#if error}
    <p class="usage-note" role="status">{error}</p>
  {:else if processes.length === 0}
    <p class="usage-note">Waiting for process memory readings.</p>
  {/if}
  {#if processes.length}
    <table class="usage-compare process-memory" aria-label="Process memory">
      <thead>
        <tr>
          <th scope="col">Process</th>
          <th scope="col">PID</th>
          <th scope="col">Memory</th>
          <th scope="col">Budget</th>
        </tr>
      </thead>
      <tbody>
        {#each processes as process (process.pid)}
          <tr>
            <th scope="row">{process.role}</th>
            <td>{process.pid}</td>
            <td>{process.memory_mib} MiB</td>
            <td>{process.budget_mib} MiB</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</section>

<style>
  h3 {
    margin: 0;
  }
</style>
