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
  // The parent passes a field of the layout snapshot, which is replaced on every publish.
  // Reading the prop in the effect would rerun it each time: the panel was cleared and
  // refetched several times a second, and the sidebar below it jumped. A derived only
  // changes when the project does.
  const currentProject = $derived(projectIdentifier);

  $effect(() => {
    const project = currentProject;
    view = null;
    error = null;
    if (project === null) return;
    let stopped = false;
    const load = async () => {
      try {
        const next = await continuousDispatchCommands.get(project);
        if (!stopped && !pending) {
          view = next;
          // A failed read is only current until the next one succeeds. Right after
          // startup the server is not up yet, and that error stayed on the panel.
          error = null;
        }
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
    <!-- The reference's stepper, as in the PR duty dialog. A number field was 24px wide and
         the browser's spin arrows took most of it, so the count was cut off. -->
    <div class="dispatch-target">
      Keep
      <div class="stepper" role="group" aria-label="Agents to keep busy">
        <button
          type="button"
          aria-label="Fewer agents"
          disabled={pending || view.slots === 0}
          onclick={() => view && void update(view.slots - 1, view.paused)}
          >−</button
        >
        <span>{view.slots}</span>
        <button
          type="button"
          aria-label="More agents"
          disabled={pending}
          onclick={() => view && void update(view.slots + 1, view.paused)}
          >+</button
        >
      </div>
      agents busy
    </div>
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
  .dispatch-target,
  .dispatch-status {
    display: flex;
    align-items: center;
    gap: var(--duty-agents-gap);
    margin-bottom: var(--duty-agents-gap);
  }
  .stepper {
    display: inline-flex;
    align-items: center;
    border: var(--duty-stepper-border);
    border-radius: var(--duty-stepper-border-radius);
  }
  .stepper button {
    width: var(--duty-stepper-button-width);
    height: var(--duty-stepper-button-height);
    color: var(--text-2);
  }
  .stepper button:hover:not(:disabled) {
    color: var(--text);
    background: var(--surface-3);
  }
  .stepper button:disabled {
    color: var(--muted);
  }
  .stepper span {
    min-width: var(--duty-stepper-count-min-width);
    text-align: center;
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
