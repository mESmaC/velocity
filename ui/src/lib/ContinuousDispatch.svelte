<script lang="ts">
  import { continuousDispatchCommands, type DispatchView } from './ipc';

  let {
    projectIdentifier,
    compact = false,
  }: {
    projectIdentifier: number | null;
    compact?: boolean;
  } = $props();

  let view = $state<DispatchView | null>(null);
  let pending = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    const project = projectIdentifier;
    view = null;
    error = null;
    if (project === null) return;
    let stopped = false;
    const load = async () => {
      try {
        const next = await continuousDispatchCommands.get(project);
        if (!stopped && !pending) view = next;
      } catch (reason) {
        if (!stopped) error = String(reason);
      }
    };
    void load();
    const timer = globalThis.setInterval(() => void load(), 2000);
    return () => {
      stopped = true;
      globalThis.clearInterval(timer);
    };
  });

  async function update(slots: number, paused: boolean) {
    const project = projectIdentifier;
    if (project === null || pending || !Number.isInteger(slots) || slots < 0)
      return;
    pending = true;
    error = null;
    try {
      const next = await continuousDispatchCommands.set(project, slots, paused);
      if (projectIdentifier === project) view = next;
    } catch (reason) {
      if (projectIdentifier === project) error = String(reason);
    } finally {
      pending = false;
    }
  }
</script>

{#if view?.available}
  <section
    class="dispatch-panel"
    class:compact
    aria-label="Continuous dispatch"
  >
    <div class="section-h">Continuous dispatch</div>
    <label>
      Keep
      <input
        type="number"
        min="0"
        step="1"
        aria-label="Keep N agents busy"
        value={view.slots}
        disabled={pending}
        onkeydown={(event) => {
          if (event.key === 'Enter') event.currentTarget.blur();
        }}
        onchange={(event) =>
          void update(event.currentTarget.valueAsNumber, view?.paused ?? false)}
      />
      agents busy
    </label>
    <div class="dispatch-status">
      <span
        >{view.busy} busy · {view.slots} slots{view.paused
          ? ' · paused'
          : ''}</span
      >
      <button
        class="btn"
        type="button"
        disabled={pending || view.slots === 0}
        onclick={() => view && void update(view.slots, !view.paused)}
        >{view.paused ? 'Resume' : 'Pause'}</button
      >
    </div>
    {#if !compact}
      <div class="section-h">Next issues</div>
      {#each view.next as issue (issue.number)}
        <div class="dispatch-issue">#{issue.number} · {issue.title}</div>
      {:else}
        <div class="dispatch-issue">No available issues</div>
      {/each}
    {/if}
    {#if error || view.error}<p role="status">{error || view.error}</p>{/if}
  </section>
{/if}

<style>
  .dispatch-panel {
    padding: var(--duty-agent-card-padding);
    border: var(--duty-agent-card-border);
    border-radius: var(--duty-agent-card-border-radius);
    background: var(--duty-agent-card-background);
    font-size: var(--duty-agent-doing-font-size);
  }
  .section-h {
    font-size: var(--duty-section-heading-font-size);
    margin-bottom: var(--duty-section-heading-margin-bottom);
  }
  .compact {
    margin-bottom: var(--duty-agents-gap);
  }
  label,
  .dispatch-status {
    display: flex;
    align-items: center;
    gap: var(--duty-agents-gap);
    margin-bottom: var(--duty-agents-gap);
  }
  input {
    width: var(--duty-stepper-button-width);
    min-width: var(--duty-stepper-count-min-width);
    color: var(--text);
    background: var(--surface-3);
    border: var(--duty-stepper-border);
    border-radius: var(--duty-stepper-border-radius);
    font: var(--duty-stepper-count-font);
  }
  .dispatch-status {
    flex-wrap: wrap;
    color: var(--text-2);
  }
  .dispatch-issue {
    padding: var(--duty-event-padding);
  }
  p {
    color: var(--text-2);
  }
</style>
