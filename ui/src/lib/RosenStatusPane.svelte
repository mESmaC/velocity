<script lang="ts">
  import {
    collapseCommands,
    onRosenStatus,
    releaseCommands,
    watchRosenStatus,
    type BaseCi,
    type CollapseMirrorHealth,
    type LayoutAction,
    type MirrorBehind,
    type RosenStatusChanged,
  } from './ipc';
  import Icon from './Icon.svelte';
  import PaneState from './PaneState.svelte';
  import RosenChangeRow from './RosenChangeRow.svelte';

  let {
    project,
    collapse,
    health = null,
    behind = null,
    ci = null,
    send,
  }: {
    project: number | null;
    // The project has a Collapse mirror, so landed changes offer Open on Collapse (#429).
    collapse: boolean;
    // Collapse's last land and downstream forwarder for that mirror (#427).
    health?: CollapseMirrorHealth | null;
    // The project's base trails its mirror while a catch-up is pending (#520).
    behind?: MirrorBehind | null;
    // GitHub CI on the mirrored base (#507), shown beside the integration state.
    ci?: BaseCi | null;
    send: (action: LayoutAction) => void;
  } = $props();

  const forwarderLabels: Record<CollapseMirrorHealth['forwarder'], string> = {
    Unconfigured: 'no downstream remote',
    Synced: 'in sync',
    CatchingUp: 'catching up',
    Retrying: 'retrying',
    Stopped: 'stopped · needs a person',
    Unavailable: 'state unavailable',
  };

  function forwarderLine(health: CollapseMirrorHealth) {
    let line = `Forwarder ${forwarderLabels[health.forwarder]}`;
    if (health.forwarder === 'CatchingUp' && health.behind > 0)
      line += ` · ${health.behind} lands behind`;
    if (health.forwarder === 'Retrying' && health.next_attempt)
      line += ` · next attempt ${new Date(health.next_attempt).toLocaleTimeString()}`;
    return line;
  }

  function ciLine(ci: BaseCi) {
    const at = ci.commit ? ` at ${ci.commit.slice(0, 10)}` : '';
    if (ci.state === 'Failed')
      return `GitHub CI failing${at}${ci.running ? ' · new run in progress' : ''}`;
    if (ci.state === 'Running') return 'GitHub CI running';
    if (ci.state === 'Green') return `GitHub CI green${at}`;
    return 'GitHub CI has not run';
  }

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

  // The rail passes a field of the layout snapshot, which is replaced on every publish.
  // Reading the prop in the effect reran it each time: the pane blanked to "Loading",
  // resubscribed and redrew several times a second (#491). A derived only changes
  // when the project does.
  const currentProject = $derived(project);

  $effect(() => {
    const current = currentProject;
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

  // The state feed line of the Mirror section (#517). Rosen records which event
  // Collapse last acknowledged, so the time is when that event happened.
  function feedLine(sentAt: string | null | undefined) {
    const at = sentAt ? Number(sentAt) : Number.NaN;
    return Number.isFinite(at)
      ? `State feed last sent ${new Date(at).toLocaleString()}`
      : 'State feed has sent nothing yet';
  }

  // Send Collapse its live state: Rust shares the push token with the feed and lands
  // the rosen.toml `collapse` root; a toast says how it went.
  let linking = $state(false);
  async function sendLiveState() {
    if (project === null || linking) return;
    linking = true;
    try {
      await collapseCommands.sendLiveState(project);
    } catch (reason) {
      error = String(reason);
    } finally {
      linking = false;
    }
  }

  async function openOnCollapse(commit: string) {
    if (project === null) return;
    try {
      await collapseCommands.open(project, commit);
    } catch (reason) {
      error = String(reason);
    }
  }

  // Cut release (#431): the version to publish and, optionally, a landed commit the
  // last green commit covers. Rust checks both and starts the release workflow.
  let releaseVersion = $state('');
  let releaseCommit = $state('');
  let releasing = $state(false);
  let releaseNote = $state('');

  async function cutRelease() {
    if (project === null || releasing) return;
    releasing = true;
    releaseNote = 'Checking green integration status on GitHub…';
    try {
      const cut = await releaseCommands.cut(
        project,
        releaseVersion,
        releaseCommit.trim() || null,
      );
      releaseNote =
        cut.note ||
        `Release workflow started for ${cut.tag} at ${cut.commit.slice(0, 10)}`;
      releaseVersion = '';
      releaseCommit = '';
    } catch (reason) {
      releaseNote = String(reason);
    } finally {
      releasing = false;
    }
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
                class="section-toggle"
                aria-expanded={!preferences.closed.includes(section)}
                onclick={() => toggle(section)}
              >
                <Icon name="chevron" />
                {section}
              </button>
              {#if index > 0}<button
                  class="move-up"
                  title="Move up"
                  aria-label={'Move ' + section + ' up'}
                  onclick={() => moveUp(index)}><Icon name="arrow" /></button
                >{/if}
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
                {#if ci}
                  <p
                    class:failing={ci.state === 'Failed'}
                    title={ci.commit ?? undefined}
                  >
                    {ciLine(ci)}
                  </p>
                  {#each ci.failing as job, index (index)}
                    <p class="hint">{job}</p>
                  {/each}
                {/if}
                {#if status.base.bisecting}<p>Bisect in progress</p>{/if}
                {#if status.base.culprit}<p title={status.base.culprit}>
                    Culprit {status.base.culprit.slice(0, 10)}
                  </p>{/if}
                {#if status.base.revert}<p title={status.base.revert}>
                    Revert {status.base.revert.slice(0, 10)}
                  </p>{/if}
                {#if status.base.green || status.base.tip}
                  <form
                    class="release"
                    aria-label="Cut release"
                    onsubmit={(event) => {
                      event.preventDefault();
                      void cutRelease();
                    }}
                  >
                    <input
                      aria-label="Release version"
                      placeholder="Version, e.g. 0.1.0-beta.2"
                      bind:value={releaseVersion}
                      disabled={releasing}
                    />
                    <input
                      class="mono"
                      aria-label="Commit to release"
                      title="A landed commit to release. Empty releases the newest commit with a green integration build."
                      placeholder={status.base.green
                        ? 'Commit · default last green ' +
                          status.base.green.slice(0, 10)
                        : 'Commit · default newest green'}
                      bind:value={releaseCommit}
                      disabled={releasing}
                    />
                    <button
                      type="submit"
                      class="btn"
                      disabled={releasing || !releaseVersion.trim()}
                      >{releasing ? 'Starting release…' : 'Cut release'}</button
                    >
                  </form>
                  {#if releaseNote}<p class="hint" role="status">
                      {releaseNote}
                    </p>{/if}
                {/if}
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
                  {#if behind}
                    <p>
                      Behind the mirror by {behind.commits}
                      {behind.commits === 1 ? 'commit' : 'commits'} · {behind.catching_up
                        ? 'catching up…'
                        : 'catch-up pending'}
                    </p>
                  {/if}
                  <p class="hint" title={status.mirror.pushed ?? undefined}>
                    Last pushed {status.mirror.pushed?.slice(0, 10) ?? '—'}
                  </p>
                  {#if status.mirror.error}<p>{status.mirror.error}</p>{/if}
                  {#if status.mirror.collapse && health}
                    <p class="hint">
                      Last land on Collapse {health.last_land_at
                        ? new Date(health.last_land_at).toLocaleString()
                        : '—'}{health.last_land_agent
                        ? ` · ${health.last_land_agent}`
                        : ''}
                    </p>
                    <p
                      class:hint={health.forwarder !== 'Stopped' &&
                        !health.archived}
                    >
                      {health.archived
                        ? 'Archived on Collapse · lands are refused'
                        : forwarderLine(health)}
                    </p>
                    {#if health.message && health.forwarder !== 'Synced'}<p
                        class="hint"
                      >
                        {health.message}
                      </p>{/if}
                  {:else if status.mirror.collapse}<p class="hint">
                      Forwarder status unavailable · connect Collapse in
                      Settings
                    </p>{/if}
                  {#if status.mirror.collapse}
                    {#if !status.mirror.live_state}
                      <p>
                        Collapse receives commits only · no live state is sent
                      </p>
                    {:else if status.mirror.feed_error}
                      <p class="failing">State feed failing</p>
                      <p class="hint">{status.mirror.feed_error}</p>
                      <p class="hint">
                        {feedLine(status.mirror.feed_sent_at)}
                      </p>
                    {:else}
                      <p class="hint">
                        {feedLine(status.mirror.feed_sent_at)}
                      </p>
                    {/if}
                    {#if !status.mirror.live_state || status.mirror.feed_error}
                      <div class="feed-action">
                        <button
                          class="mini"
                          disabled={linking}
                          title="Give Rosen's state feed the mirror's push token and land the collapse root in rosen.toml"
                          onclick={() => void sendLiveState()}
                          >{linking
                            ? 'Linking…'
                            : 'Send Collapse its live state'}</button
                        >
                      </div>
                    {/if}
                  {/if}
                {:else}
                  <p class="hint">No mirror connected.</p>
                {/if}
              {:else}
                {#each status.changes as change (change.id)}
                  <RosenChangeRow
                    {change}
                    {open}
                    openOnCollapse={collapse ? openOnCollapse : null}
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
  .panel-body {
    padding: var(--sp-2) var(--sp-3);
  }
  .status-note {
    margin: var(--sp-3) var(--sp-3) var(--sp-2);
    color: var(--muted);
    font-size: var(--fs-sm);
  }
  .status-section {
    padding: var(--sp-2) 0 var(--sp-4);
  }
  .status-section + .status-section {
    border-top: 1px solid var(--line-dim);
  }
  .section-heading {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--sp-2);
    min-height: 28px;
  }
  .section-toggle {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-3);
    flex: 1;
    min-width: 0;
    height: 26px;
    padding: 0 var(--sp-3);
    border-radius: var(--r-sm);
    color: var(--text);
    font-size: var(--fs-md);
    font-weight: var(--fw-semibold);
    text-align: left;
  }
  .section-toggle:hover {
    background: var(--surface-2);
  }
  .section-toggle :global(.icon) {
    width: 10px;
    height: 10px;
    color: var(--muted);
    transition: transform 0.15s;
  }
  .section-toggle[aria-expanded='false'] :global(.icon) {
    transform: rotate(-90deg);
  }
  /* Reordering is a rare action: the control appears when the section is hovered. */
  .move-up {
    display: inline-grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--r-sm);
    color: var(--muted);
    opacity: 0;
  }
  .move-up :global(.icon) {
    width: var(--icon-sm);
    height: var(--icon-sm);
  }
  .status-section:hover .move-up,
  .move-up:focus-visible {
    opacity: 1;
  }
  .move-up:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  p {
    margin: var(--sp-1) 0 0;
    padding: 0 var(--sp-3) 0 calc(var(--sp-3) + 10px + var(--sp-3));
    font-size: var(--fs-sm);
    line-height: var(--lh);
    color: var(--text-2);
    overflow-wrap: anywhere;
  }
  .hint {
    color: var(--muted);
  }
  .failing {
    color: var(--err);
  }
  .release {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--sp-2);
    margin: var(--sp-3) 0 0;
    padding: 0 var(--sp-3) 0 calc(var(--sp-3) + 10px + var(--sp-3));
  }
  .feed-action {
    margin: var(--sp-2) 0 0;
    padding: 0 var(--sp-3) 0 calc(var(--sp-3) + 10px + var(--sp-3));
  }
  .release input {
    width: 100%;
    font-size: var(--fs-sm);
  }
  .mono {
    font-family: var(--mono);
  }
</style>
