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
  {#each processes as process (process.pid)}
    <div class="usage-window">
      <div class="usage-window-line">
        <b>{process.role}</b>
        <span class="usage-window-label">PID {process.pid}</span>
        <span class="usage-window-reset">
          {process.memory_mib} / {process.budget_mib} MiB
        </span>
      </div>
    </div>
  {/each}
</section>

<style>
  h3 {
    margin: 0;
  }
</style>
