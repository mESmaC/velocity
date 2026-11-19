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
    <div class="dispatch-head">
      <span class="section-h">Continuous dispatch</span>
      <span
        class="tag"
        data-tone={view.slots === 0 ? undefined : view.paused ? 'warn' : 'ok'}
        >{view.slots === 0 ? 'Off' : view.paused ? 'Paused' : 'On'}</span
      >
    </div>
    <!-- A stepper, not a number field: a field 24px wide lost most of its width to
         the browser's spin arrows, so the count was cut off. -->
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
      busy
      <span class="spacer"></span>
      <button
        class="mini"
        type="button"
        disabled={pending || view.slots === 0}
        onclick={() => view && void update(view.slots, !view.paused)}
        >{view.paused ? 'Resume' : 'Pause'}</button
      >
    </div>
    <div class="dispatch-status">
      {view.busy} busy · {view.slots}
      {view.slots === 1 ? 'slot' : 'slots'}
    </div>
    {#if !compact}
      <div class="section-h next">Next work</div>
      {#each view.next as item (item.key)}
        <div class="dispatch-issue">
          {item.label} · {item.title}{item.unblocks
            ? ` · unblocks ${item.unblocks}`
            : ''}
        </div>
      {:else}
        <div class="dispatch-issue">No available work</div>
      {/each}
    {/if}
    {#if view.integration_notice}<p role="status">
        {view.integration_notice}
      </p>{/if}
    {#if error || view.error}<p role="status">{error || view.error}</p>{/if}
  </section>
{/if}

<style>
  .dispatch-panel {
    display: grid;
    gap: var(--sp-3);
    padding: var(--sp-5) var(--sp-5) var(--sp-4);
    border: 1px solid var(--line-dim);
    border-radius: var(--r-md);
    background: var(--card);
    font-size: var(--fs-md);
    color: var(--text-2);
  }
  .compact {
    margin-bottom: var(--sp-3);
  }
  .dispatch-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-4);
  }
  .section-h {
    color: var(--text);
    font-size: var(--fs-md);
    font-weight: var(--fw-semibold);
  }
  .section-h.next {
    margin-top: var(--sp-3);
    font-size: var(--fs-sm);
  }
  .dispatch-target {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
  .stepper {
    display: inline-flex;
    align-items: center;
    height: var(--h-sm);
    border: 1px solid var(--line-mid);
    border-radius: var(--r-sm);
    background: rgba(0, 0, 0, 0.3);
    overflow: hidden;
  }
  .stepper button {
    width: 22px;
    height: 100%;
    color: var(--text-2);
  }
  .stepper button:hover:not(:disabled) {
    color: var(--text);
    background: var(--surface-3);
  }
  .stepper span {
    min-width: 20px;
    text-align: center;
    color: var(--text);
    font-weight: var(--fw-semibold);
  }
  .dispatch-status {
    color: var(--muted);
    font-size: var(--fs-sm);
  }
  .dispatch-issue {
    padding: var(--sp-1) 0;
    font-size: var(--fs-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  p {
    margin: 0;
    color: var(--warn);
    font-size: var(--fs-sm);
  }
</style>
