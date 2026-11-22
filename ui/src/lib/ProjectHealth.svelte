<script lang="ts">
  import { SvelteMap } from 'svelte/reactivity';
  import Icon from './Icon.svelte';
  import {
    collapseCommands,
    onProjectHealth,
    projectHealthCommands,
    type HealthSeverity,
    type LayoutAction,
    type ProjectHealthItem,
  } from './ipc';

  // Project health notifications (#509). A bell in the title bar counts the active
  // signals and lists them in a dropdown. A signal that appears or changes while the
  // project is open also pops out under the bell for a few seconds. Signals already
  // active when a project loads only set the badge, so switching projects doesn't
  // replay old pop-outs.
  let {
    project = null,
    send,
    onForwardingWarning,
  }: {
    project: number | null;
    send: (action: LayoutAction) => void;
    onForwardingWarning?: (item: ProjectHealthItem | null) => void;
  } = $props();

  const POPOUT_MS = 8000;
  const MAX_POPOUTS = 3;
  const RANK: Record<HealthSeverity, number> = {
    Info: 0,
    Warning: 1,
    Error: 2,
  };

  let items = $state.raw<ProjectHealthItem[]>([]);
  let popouts = $state.raw<ProjectHealthItem[]>([]);
  let menu = $state<HTMLElement>();
  // What each signal said when last shown. The timestamp is left out, so a check
  // that re-confirms the same signal doesn't pop it out again.
  let seen = new Map<string, string>();
  const timers = new SvelteMap<string, ReturnType<typeof setTimeout>>();
  const currentProject = $derived(project);
  const worst = $derived(
    items.reduce<HealthSeverity | null>(
      (top, item) =>
        top === null || RANK[item.severity] > RANK[top] ? item.severity : top,
      null,
    ),
  );

  const fingerprint = (item: ProjectHealthItem) =>
    `${item.severity}|${item.signal}|${item.action}|${item.action_url ?? ''}`;

  function safeExternalUrl(url: string | null | undefined): string | null {
    if (!url) return null;
    try {
      const parsed = new globalThis.URL(url);
      return parsed.protocol === 'https:' ? parsed.toString() : null;
    } catch {
      return null;
    }
  }

  function publishForwardingWarning(next: ProjectHealthItem[]) {
    onForwardingWarning?.(
      next.find((item) => item.kind === 'CollapseForwarding') ?? null,
    );
  }

  function receive(next: ProjectHealthItem[], announce: boolean) {
    const menuOpen = menu?.matches(':popover-open') ?? false;
    const fresh =
      announce && !menuOpen
        ? next.filter((item) => seen.get(item.key) !== fingerprint(item))
        : [];
    seen = new Map(next.map((item) => [item.key, fingerprint(item)]));
    items = next;
    publishForwardingWarning(next);
    const live = new Set(next.map((item) => item.key));
    const freshKeys = new Set(fresh.map((item) => item.key));
    const kept = popouts.filter(
      (item) => live.has(item.key) && !freshKeys.has(item.key),
    );
    for (const item of popouts) if (!live.has(item.key)) stopTimer(item.key);
    popouts = [...fresh, ...kept].slice(0, MAX_POPOUTS);
    fresh.forEach((item) => startTimer(item.key));
  }

  function startTimer(key: string) {
    stopTimer(key);
    timers.set(
      key,
      setTimeout(() => dismiss(key), POPOUT_MS),
    );
  }

  function stopTimer(key: string) {
    clearTimeout(timers.get(key));
    timers.delete(key);
  }

  function dismiss(key: string) {
    stopTimer(key);
    popouts = popouts.filter((item) => item.key !== key);
  }

  function clearPopouts() {
    for (const key of [...timers.keys()]) stopTimer(key);
    popouts = [];
  }

  function openThread(threadId: string) {
    menu?.hidePopover();
    clearPopouts();
    send({ OpenThread: threadId });
  }

  function openActionUrl(event: globalThis.MouseEvent, url: string) {
    event.preventDefault();
    menu?.hidePopover();
    clearPopouts();
    void collapseCommands.openPage(url);
  }

  $effect(() => {
    const id = currentProject;
    items = [];
    seen = new Map();
    clearPopouts();
    publishForwardingWarning([]);
    if (id === null) return;
    let stopped = false;
    // The first view, from the read or an early event, only fills the badge.
    let loaded = false;
    void projectHealthCommands
      .get(id)
      .then((view) => {
        if (stopped || loaded || view.project_identifier !== id) return;
        receive(view.items, false);
        loaded = true;
      })
      .catch(() => {
        // The next live event fills the list if the initial read fails.
      });
    const listening = onProjectHealth((event) => {
      if (stopped || event.payload.view.project_identifier !== id) return;
      receive(event.payload.view.items, loaded);
      loaded = true;
    });
    return () => {
      stopped = true;
      clearPopouts();
      void listening.then((unlisten) => unlisten());
    };
  });

  function formatWhen(actedAtMs: string): string {
    const ms = Number(actedAtMs);
    if (!Number.isFinite(ms) || ms <= 0) return '';
    return new Date(ms).toLocaleTimeString([], {
      hour: '2-digit',
      minute: '2-digit',
    });
  }

  const summary = $derived(
    items.length === 0
      ? 'Project health: all clear'
      : `Project health: ${items.length} signal${items.length === 1 ? '' : 's'}`,
  );
</script>

{#snippet healthItem(item: ProjectHealthItem, popout: boolean)}
  {@const actionUrl = safeExternalUrl(item.action_url)}
  <span class="health-dot" aria-hidden="true"></span>
  <div class="health-body">
    <div class="health-headline">
      <strong class="health-signal">{item.signal}</strong>
      {#if formatWhen(item.acted_at_ms)}
        <time
          class="health-when"
          datetime={new Date(Number(item.acted_at_ms)).toISOString()}
          title={new Date(Number(item.acted_at_ms)).toLocaleString()}
          >{formatWhen(item.acted_at_ms)}</time
        >
      {/if}
    </div>
    <div class="health-action">{item.action}</div>
    {#if item.detail && !popout}
      <div class="health-detail">{item.detail}</div>
    {/if}
    {#if item.thread_identifier}
      {@const threadId = item.thread_identifier}
      <button
        class="mini health-open"
        type="button"
        onclick={() => openThread(threadId)}>Open thread</button
      >
    {/if}
    {#if actionUrl}
      <a
        class="mini health-open"
        href={actionUrl}
        target="_blank"
        rel="noopener noreferrer"
        onclick={(event) => openActionUrl(event, actionUrl)}
        >{item.action_label ?? 'Open'} ↗</a
      >
    {/if}
  </div>
{/snippet}

<button
  class="icon-btn health-bell"
  type="button"
  popovertarget="health-menu"
  aria-haspopup="dialog"
  data-severity={worst}
  title={summary}
  aria-label={summary}
  onclick={clearPopouts}
  ><Icon name="bell" />{#if items.length > 0}<span
      class="health-count"
      aria-hidden="true">{items.length}</span
    >{/if}</button
>

<div
  bind:this={menu}
  id="health-menu"
  class="health-menu"
  popover="auto"
  role="dialog"
  aria-label="Project health"
>
  <div class="health-menu-head">Project health</div>
  {#each items as item (item.key)}
    <div class="health-row" data-severity={item.severity}>
      {@render healthItem(item, false)}
    </div>
  {:else}
    <p class="health-empty">All clear. Velocity reports problems here.</p>
  {/each}
</div>

{#if popouts.length > 0}
  <div class="health-popouts" role="status" aria-live="polite">
    {#each popouts as item (item.key)}
      <div
        class="health-row health-popout"
        data-severity={item.severity}
        role="group"
        aria-label={item.signal}
        onpointerenter={() => stopTimer(item.key)}
        onpointerleave={() => startTimer(item.key)}
      >
        {@render healthItem(item, true)}
        <button
          class="health-dismiss"
          type="button"
          aria-label="Dismiss"
          title="Dismiss"
          onclick={() => dismiss(item.key)}>×</button
        >
      </div>
    {/each}
  </div>
{/if}

<style>
  .health-bell {
    position: relative;
    color: var(--muted);
  }
  .health-bell[data-severity='Info'] {
    color: var(--info);
  }
  .health-bell[data-severity='Warning'] {
    color: var(--warn);
  }
  .health-bell[data-severity='Error'] {
    color: var(--err);
  }
  .health-count {
    position: absolute;
    top: 0;
    right: 0;
    min-width: 14px;
    height: 14px;
    padding: 0 var(--sp-1);
    border-radius: var(--r-pill);
    /* The digit sits on the severity colour, so it takes the window black. */
    background: var(--muted);
    color: var(--bg);
    font-size: 10px;
    font-weight: var(--fw-semibold);
    line-height: 14px;
    text-align: center;
    font-variant-numeric: tabular-nums;
  }
  .health-bell[data-severity='Info'] .health-count {
    background: var(--info);
  }
  .health-bell[data-severity='Warning'] .health-count {
    background: var(--warn);
  }
  .health-bell[data-severity='Error'] .health-count {
    background: var(--err);
  }

  .health-menu,
  .health-popout {
    border: 1px solid var(--pop-edge);
    background: var(--glass-pop);
    box-shadow: var(--shadow-pop);
    backdrop-filter: var(--blur-pop);
    -webkit-backdrop-filter: var(--blur-pop);
    color: var(--text-2);
    font-size: var(--fs-sm);
    line-height: var(--lh);
  }
  .health-menu {
    position: fixed;
    inset: var(--titlebar-h) var(--gutter) auto auto;
    width: min(440px, calc(100vw - 2 * var(--gutter)));
    max-height: min(70vh, 560px);
    margin: 0;
    padding: var(--sp-3);
    overflow-y: auto;
    border-radius: var(--r-lg);
  }
  .health-menu:popover-open {
    display: grid;
    gap: var(--sp-1);
  }
  .health-menu-head {
    padding: var(--sp-2) var(--sp-4) var(--sp-3);
    color: var(--muted);
    font-size: var(--fs-xs);
    font-weight: var(--fw-semibold);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .health-empty {
    margin: 0;
    padding: var(--sp-4);
    color: var(--muted);
  }
  .health-menu .health-row {
    padding: var(--sp-4);
    border-radius: var(--r-md);
  }
  .health-menu .health-row:hover {
    background: var(--surface-2);
  }

  .health-popouts {
    position: fixed;
    top: calc(var(--titlebar-h) + var(--sp-2));
    right: var(--gutter);
    z-index: 40;
    display: grid;
    gap: var(--sp-3);
    width: min(380px, calc(100vw - 2 * var(--gutter)));
    pointer-events: none;
  }
  .health-popout {
    position: relative;
    padding: var(--sp-4) var(--sp-8) var(--sp-4) var(--sp-5);
    border-radius: var(--r-md);
    pointer-events: auto;
    animation: health-pop-in var(--motion-duration-base) var(--motion-ease-out);
  }
  .health-dismiss {
    position: absolute;
    top: var(--sp-2);
    right: var(--sp-3);
    color: var(--muted);
    font-size: var(--fs-md);
    line-height: 1;
  }
  .health-dismiss:hover {
    color: var(--text);
  }
  @keyframes health-pop-in {
    from {
      opacity: 0;
      transform: translateY(calc(-1 * var(--sp-3)))
        scale(var(--motion-scale-from));
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .health-popout {
      animation: none;
    }
  }

  .health-row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-4);
    min-width: 0;
  }
  .health-dot {
    flex: none;
    width: var(--sp-3);
    height: var(--sp-3);
    margin-top: var(--sp-2);
    border-radius: var(--r-pill);
    background: var(--info);
  }
  .health-row[data-severity='Warning'] .health-dot {
    background: var(--warn);
  }
  .health-row[data-severity='Error'] .health-dot {
    background: var(--err);
  }
  .health-body {
    display: grid;
    gap: var(--sp-1);
    flex: 1;
    min-width: 0;
  }
  .health-headline {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-4);
  }
  .health-signal {
    color: var(--text);
    font-weight: var(--fw-semibold);
  }
  .health-row[data-severity='Warning'] .health-signal {
    color: var(--warn);
  }
  .health-row[data-severity='Error'] .health-signal {
    color: var(--err);
  }
  .health-when {
    flex: none;
    color: var(--muted);
    font-size: var(--fs-xs);
    font-variant-numeric: tabular-nums;
  }
  .health-action {
    color: var(--text-2);
  }
  .health-detail {
    color: var(--muted);
    font-family: var(--mono);
    font-size: var(--fs-xs);
    overflow-wrap: anywhere;
  }
  .health-open {
    justify-self: start;
    margin-top: var(--sp-2);
  }
</style>
