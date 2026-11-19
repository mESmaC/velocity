<script lang="ts">
  import {
    onRosenStatus,
    watchRosenStatus,
    type LayoutAction,
    type RosenStatusChanged,
  } from './ipc';
  import PaneState from './PaneState.svelte';
  import RosenChangeRow from './RosenChangeRow.svelte';

  let {
    project,
    send,
  }: { project: number | null; send: (action: LayoutAction) => void } =
    $props();

  const sections = [
    'Server',
    'Base',
    'Landing',
    'Claims and conflicts',
    'Warm pool',
    'Mirror',
    'Changes',
  ] as const;
  type Section = (typeof sections)[number];
  const storageKey = 'velocity-rosen-status-sections';

  function savedSections() {
    try {
      const saved: { order?: string[]; closed?: string[] } = JSON.parse(
        window.localStorage.getItem(storageKey) ?? '{}',
      );
      const order = [
        ...new Set(
          saved.order?.filter((value): value is Section =>
            sections.some((section) => section === value),
          ) ?? [],
        ),
      ];
      return {
        order: [
          ...order,
          ...sections.filter((section) => !order.includes(section)),
        ],
        closed: sections.filter((section) => saved.closed?.includes(section)),
      };
    } catch {
      return { order: [...sections], closed: [] };
    }
  }

  let preferences = $state(savedSections());
  let frame = $state<RosenStatusChanged | null>(null);
  let error = $state('');
  let enabled = $state(true);
  const status = $derived(frame?.status);
  const openChanges = $derived(
    status?.changes.filter(
      (change) =>
        change.phase !== 'landed' &&
        change.phase !== 'green' &&
        change.phase !== 'abandoned',
    ).length,
  );

  $effect(() => {
    const current = project;
    let stopped = false;
    let stopListening: (() => void) | undefined;
    frame = null;
    error = '';
    enabled = current !== null;
    async function connect() {
      const unlisten = await onRosenStatus(({ payload }) => {
        if (!stopped && payload.project_identifier === current) {
          frame = payload;
          error = payload.error ?? '';
        }
      });
      if (stopped) {
        unlisten();
        return;
      }
      stopListening = unlisten;
      const supported = await watchRosenStatus(current);
      if (!stopped) enabled = supported;
    }
    void connect().catch((reason: unknown) => {
      if (!stopped) error = String(reason);
    });
    return () => {
      stopped = true;
      stopListening?.();
    };
  });

  function persist() {
    try {
      window.localStorage.setItem(storageKey, JSON.stringify(preferences));
    } catch {
      // Settings still apply for this window when local storage is unavailable.
    }
  }

  function moveUp(index: number) {
    const moved = preferences.order[index];
    preferences.order.splice(index, 1);
    preferences.order.splice(index - 1, 0, moved);
    persist();
  }

  function toggle(section: Section) {
    preferences.closed = preferences.closed.includes(section)
      ? preferences.closed.filter((value) => value !== section)
      : [...preferences.closed, section];
    persist();
  }

  function open(identifier: string) {
    send({ OpenThread: identifier });
  }

  const seconds = new Intl.NumberFormat(undefined, {
    maximumFractionDigits: 2,
  });
</script>

{#if enabled && project !== null}
  <section class="panel" aria-label="Rosen status">
    <div class="panel-h">
      <span class="label">Rosen status</span>
      {#if openChanges !== undefined}<span class="count">{openChanges}</span
        >{/if}
    </div>
    <div class="panel-body">
      <PaneState
        name="Rosen status"
        loading={!frame && !error}
        {error}
        automatic={false}
        network={false}
        retry={async () => {
          await watchRosenStatus(project);
        }}
      />
      {#if status?.before_land}
        <p class="status-note" role="status">
          Switch to <code>review = "after-land"</code> in
          <code>rosen.toml</code> to enable automatic landing.
        </p>
      {/if}
      {#if status}
        {#each preferences.order as section, index (section)}
          <section class="status-section" aria-label={section}>
            <div class="section-heading">
              <button
                class="mini"
                aria-expanded={!preferences.closed.includes(section)}
                onclick={() => toggle(section)}
              >
                {preferences.closed.includes(section) ? '›' : '⌄'}
                {section}
              </button>
              <button
                class="mini"
                aria-label={'Move ' + section + ' up'}
                disabled={index === 0}
                onclick={() => moveUp(index)}>↑</button
              >
            </div>
            {#if !preferences.closed.includes(section)}
              {#if section === 'Server'}
                <p>
                  {status.server
                    ? 'Running · ' + status.server
                    : 'Offline · Reconnecting…'}
                </p>
                {#if status.server}
                  <p class="hint">
                    {status.machines.length} connected {status.machines
                      .length === 1
                      ? 'machine'
                      : 'machines'}
                    {#if status.machines.length}
                      · {status.machines.join(' · ')}{/if}
                  </p>
                  <p class="hint">
                    Run queue · {status.local.holders.join(', ') || 'idle'} · {status
                      .local.waiting} waiting
                  </p>
                {/if}
              {:else if section === 'Base'}
                <p title={status.base.tip ?? undefined}>
                  Tip {status.base.tip?.slice(0, 10) ?? '—'}
                </p>
                <p title={status.base.green ?? undefined}>
                  Last green {status.base.green?.slice(0, 10) ?? '—'}
                </p>
                {#if status.base.bisecting}<p>Bisect in progress</p>{/if}
                {#if status.base.culprit}<p title={status.base.culprit}>
                    Culprit {status.base.culprit.slice(0, 10)}
                  </p>{/if}
                {#if status.base.revert}<p title={status.base.revert}>
                    Revert {status.base.revert.slice(0, 10)}
                  </p>{/if}
              {:else if section === 'Landing'}
                <p>
                  {status.landing.lands_last_hour} in the last hour
                  {#if typeof status.landing.median_ms === 'number'}
                    · Median {seconds.format(status.landing.median_ms / 1000)}s
                    from naming
                  {/if}
                </p>
                <p class="hint">
                  Post-land backlog · {status.landing.builds_pending} builds · {status
                    .landing.tests_pending} full tests
                </p>
              {:else if section === 'Claims and conflicts'}
                {#each status.claims as claim (`${claim.change}:${claim.kind}:${claim.paths.join()}`)}
                  <p>
                    {claim.paths.join(', ')} · {claim.owner}
                    <span class="hint">({claim.kind})</span>
                  </p>
                {/each}
                {#each status.changes as change (change.id)}
                  {#each change.overlaps.filter((overlap) => !status.changes.some((other) => other.id < change.id && other.id === overlap.change && other.overlaps.some((back) => back.change === change.id))) as overlap (overlap.change)}
                    <p>
                      Overlap · {overlap.paths.join(', ')} · {change.owner} and {overlap.owner}
                    </p>
                  {/each}
                  {#each change.conflicts as path (path)}
                    <p>Conflict · {path} · {change.owner}</p>
                  {/each}
                {/each}
                {#if !status.claims.length && !status.changes.some((change) => change.conflicts.length || change.overlaps.length)}
                  <p class="hint">No claims, overlaps or conflicts.</p>
                {/if}
              {:else if section === 'Warm pool'}
                <p>{status.local.ready} ready / target {status.local.target}</p>
              {:else if section === 'Mirror'}
                {#if status.mirror.remote}
                  <p>{status.mirror.state} · {status.mirror.remote}</p>
                  <p class="hint" title={status.mirror.pushed ?? undefined}>
                    Last pushed {status.mirror.pushed?.slice(0, 10) ?? '—'}
                  </p>
                  {#if status.mirror.error}<p>{status.mirror.error}</p>{/if}
                  {#if status.mirror.collapse}<p class="hint">
                      Forwarder status unavailable
                    </p>{/if}
                {:else}
                  <p class="hint">No mirror connected.</p>
                {/if}
              {:else}
                {#each status.changes as change (change.id)}
                  <RosenChangeRow
                    {change}
                    {open}
                    owner={frame?.owners[change.id]}
                    claims={status.claims.filter(
                      (claim) =>
                        claim.change === change.id && claim.kind === 'claim',
                    ).length}
                  />
                {:else}
                  <p class="empty">No changes yet.</p>
                {/each}
              {/if}
            {/if}
          </section>
        {/each}
      {/if}
    </div>
  </section>
{/if}

<style>
  .status-note {
    margin: 0 0 8px;
    color: var(--muted);
  }

  .status-section {
    padding: var(--rosen-section-padding);
    border-bottom: var(--rosen-section-border);
  }
  .section-heading {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  p {
    margin: var(--rosen-line-margin);
    font-size: var(--rosen-line-size);
    line-height: var(--rosen-line-height);
    overflow-wrap: anywhere;
  }
  .hint {
    color: var(--muted);
  }
</style>
