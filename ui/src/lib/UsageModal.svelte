<script lang="ts">
  import PaneState from './PaneState.svelte';
  import Icon from './Icon.svelte';
  import GraphStatusView from './GraphStatus.svelte';
  import { modal } from './motion';
  import { codeGraphCommands } from './ipc';
  import type {
    GraphComparison,
    GraphStatus,
    LayoutAction,
    ProviderColor,
    ProviderUsage,
  } from './ipc';
  import './theme/usage.css';

  // The usage modal (SPEC.md F15 Usage, F19 Measuring it): each signed-in provider's
  // subscription windows, laid out like T3 Code's usage limits, and the project's code
  // graph switch with its token and time comparison. Rust draws every row and works out
  // the comparison; the modal renders what it gets.
  let {
    providers,
    project,
    send,
    close,
  }: {
    providers: ProviderUsage[];
    project: number | null;
    send: (action: LayoutAction) => void;
    close: () => void;
  } = $props();

  let dialog: HTMLDialogElement;
  let graph = $state<GraphComparison | null>(null);
  let status = $state<GraphStatus | null>(null);
  let busy = $state(false);
  let statusError = $state('');
  let revision = $state(0);
  let error = $state('');

  const icons: Record<ProviderColor, string> = {
    Claude: 'claude',
    Codex: 'codex',
    Antigravity: 'antigravity',
    Kimi: 'kimi',
    Custom: 'agents',
  };

  $effect(() => {
    const identifier = project;
    if (identifier === null) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    async function refresh(identifier: number) {
      try {
        const results = await Promise.allSettled([
          codeGraphCommands.comparison(identifier),
          codeGraphCommands.status(identifier),
        ]);
        if (cancelled || busy) return;
        const [comparison, index] = results;
        if (comparison.status === 'fulfilled') {
          graph = comparison.value;
        } else {
          error = String(comparison.reason);
        }
        if (index.status === 'fulfilled') {
          status = index.value;
          statusError = '';
        } else {
          statusError = String(index.reason);
        }
      } finally {
        if (!cancelled)
          timer = setTimeout(() => void refresh(identifier), 2000);
      }
    }
    const refreshRevision = revision;
    void refresh(identifier);
    return () => {
      cancelled = true;
      clearTimeout(timer);
      if (revision === refreshRevision) {
        graph = null;
        status = null;
      }
    };
  });

  async function run(request: Promise<GraphComparison | GraphStatus>) {
    const identifier = project;
    busy = true;
    try {
      const updated = await request;
      if (project !== identifier) return;
      if ('enabled' in updated) {
        graph = updated;
      } else {
        status = updated;
        statusError = '';
      }
      error = '';
    } catch (failure) {
      error = String(failure);
    } finally {
      busy = false;
      revision += 1;
    }
  }

  function setEnabled(enabled: boolean) {
    if (project === null || graph?.enabled === enabled) return;
    void run(codeGraphCommands.setEnabled(project, enabled));
  }

  function retry() {
    if (project !== null) void run(codeGraphCommands.retry(project));
  }

  function reset() {
    if (project !== null) void run(codeGraphCommands.reset(project));
  }

  function openProviders() {
    close();
    send({ Request: { SettingsTab: 'providers' } });
  }
</script>

<dialog
  bind:this={dialog}
  transition:modal|global
  class="usage-dialog"
  aria-labelledby="usage-title"
  oncancel={(event) => {
    event.preventDefault();
    close();
  }}
  onclick={(event) => {
    if (event.target === dialog) close();
  }}
>
  <div class="usage-modal">
    <div class="usage-modal-head">
      <h2 id="usage-title">Usage</h2>
      <span class="label">Esc to close</span>
    </div>
    <div class="usage-modal-body">
      <section aria-label="Usage limits">
        <div class="usage-title">Usage limits</div>
        <PaneState name="Usage" />
        {#each providers as provider (provider.account_key)}
          <section class="usage-provider">
            <div class="usage-provider-head">
              <Icon
                name={icons[provider.provider]}
              />{provider.name}{#if provider.plan}<span>{provider.plan}</span
                >{/if}
            </div>
            {#each provider.windows as window (window.label)}
              <div class="usage-window" title={window.summary}>
                <div class="usage-window-line">
                  <span class="usage-window-label">{window.label}</span>
                  <span class="usage-window-reset">{window.resets_in}</span>
                </div>
                <div class="usage-window-line">
                  <span
                    class="usage-bar"
                    role="img"
                    aria-label={`${window.label}: ${window.summary}`}
                    data-provider={provider.provider}
                    ><span style={`--usage-left: ${window.percent_left}%`}
                    ></span></span
                  >
                  <b>{window.percent_left}% left</b>
                </div>
              </div>
            {/each}
            <PaneState
              name={provider.name}
              empty={!provider.windows.length}
              message="No limits reported yet · Usage refreshes automatically."
            />
          </section>
        {:else}
          <PaneState
            name="Usage"
            empty
            message="No provider is signed in · Open Settings → Providers."
          />
        {/each}
      </section>
      <hr />
      <section aria-label="Code graph">
        <div class="usage-graph-head">
          <div class="usage-title">Code graph</div>
          {#if graph}<div
              class="usage-seg"
              role="radiogroup"
              aria-label="Code graph for this project"
            >
              <button
                role="radio"
                aria-checked={graph.enabled}
                class:on={graph.enabled}
                disabled={busy}
                onclick={() => setEnabled(true)}>On</button
              ><button
                role="radio"
                aria-checked={!graph.enabled}
                class:on={!graph.enabled}
                disabled={busy}
                onclick={() => setEnabled(false)}>Off</button
              >
            </div>{/if}
        </div>
        {#if project === null}
          <p class="usage-note">Open a project to see its code graph.</p>
        {:else if graph}
          <p class="usage-note">
            {graph.enabled
              ? 'Agents started in this project get the graph tools.'
              : 'Agents started in this project work without the graph.'} Running
            agents keep what they started with. Each turn's tokens and working time
            count toward the side it ran on.
          </p>
          {#if status && !statusError}
            <GraphStatusView {status} {retry} {busy} enabled={graph.enabled} />
          {:else if statusError}
            <div class="usage-graph-foot">
              <span role="status">{statusError}</span>
              <button class="usage-button" disabled={busy} onclick={retry}
                >Retry</button
              >
            </div>
          {:else}
            <p class="usage-note" role="status">Checking the graph index…</p>
          {/if}
          <table class="usage-compare" aria-label="Graph on and off comparison">
            <thead>
              <tr>
                <th scope="col"></th>
                {#each graph.sides as side (side.label)}<th scope="col"
                    >{side.label}</th
                  >{/each}
                <th scope="col">Change</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <th scope="row">Turns</th>
                {#each graph.sides as side (side.label)}<td>{side.turns}</td
                  >{/each}
                <td>—</td>
              </tr>
              <tr>
                <th scope="row">Tokens per turn</th>
                {#each graph.sides as side (side.label)}<td title={side.note}
                    >{side.tokens_per_turn}</td
                  >{/each}
                <td>{graph.tokens_delta}</td>
              </tr>
              <tr>
                <th scope="row">Time per turn</th>
                {#each graph.sides as side (side.label)}<td
                    >{side.time_per_turn}</td
                  >{/each}
                <td>{graph.time_delta}</td>
              </tr>
              <tr>
                <th scope="row">Total tokens</th>
                {#each graph.sides as side (side.label)}<td
                    >{side.total_tokens}</td
                  >{/each}
                <td>—</td>
              </tr>
              <tr>
                <th scope="row">Total time</th>
                {#each graph.sides as side (side.label)}<td
                    >{side.total_time}</td
                  >{/each}
                <td>—</td>
              </tr>
            </tbody>
          </table>
          {#each graph.sides.filter((side) => side.note) as side (side.label)}<p
              class="usage-note"
            >
              {side.label}: {side.note}
            </p>{/each}
          <p class="usage-verdict" class:pending={!graph.conclusive}>
            {graph.verdict}
          </p>
          <div class="usage-graph-foot">
            <span>{graph.since}</span>
            <button class="usage-button" disabled={busy} onclick={reset}
              >Reset counts</button
            >
          </div>
        {/if}
        <PaneState
          name="Code graph usage"
          {error}
          loading={project !== null && !graph}
          retry={() =>
            project === null
              ? undefined
              : run(codeGraphCommands.comparison(project))}
        />
      </section>
      <hr />
      <button class="usage-link" onclick={openProviders}
        >Settings · Providers</button
      >
    </div>
  </div>
</dialog>
