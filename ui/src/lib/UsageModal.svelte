<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { pop } from './motion';
  import { codeGraphCommands } from './ipc';
  import type {
    GraphComparison,
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
  let busy = $state(false);
  let error = $state('');

  const icons: Record<ProviderColor, string> = {
    Claude: 'claude',
    Codex: 'codex',
    Antigravity: 'antigravity',
    Kimi: 'kimi',
    Custom: 'agents',
  };

  onMount(() => {
    const previous = document.activeElement;
    dialog.showModal();
    if (project !== null) void run(codeGraphCommands.comparison(project));
    return () => {
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus();
    };
  });

  async function run(request: Promise<GraphComparison>) {
    busy = true;
    try {
      graph = await request;
      error = '';
    } catch (failure) {
      error = String(failure);
    } finally {
      busy = false;
    }
  }

  function setEnabled(enabled: boolean) {
    if (project === null || graph?.enabled === enabled) return;
    void run(codeGraphCommands.setEnabled(project, enabled));
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
  in:pop|global
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
        {#each providers as provider (provider.provider + provider.name)}
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
            {#if provider.note}<p class="usage-note">{provider.note}</p>{/if}
          </section>
        {:else}
          <p class="usage-note">No provider is signed in.</p>
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
          <table class="usage-compare">
            <thead>
              <tr>
                <th scope="col"></th>
                {#each graph.sides as side (side.label)}<th scope="col"
                    >{side.label}</th
                  >{/each}
              </tr>
            </thead>
            <tbody>
              <tr>
                <th scope="row">Turns</th>
                {#each graph.sides as side (side.label)}<td>{side.turns}</td
                  >{/each}
              </tr>
              <tr>
                <th scope="row">Tokens per turn</th>
                {#each graph.sides as side (side.label)}<td title={side.note}
                    >{side.tokens_per_turn}</td
                  >{/each}
              </tr>
              <tr>
                <th scope="row">Time per turn</th>
                {#each graph.sides as side (side.label)}<td
                    >{side.time_per_turn}</td
                  >{/each}
              </tr>
              <tr>
                <th scope="row">Total tokens</th>
                {#each graph.sides as side (side.label)}<td
                    >{side.total_tokens}</td
                  >{/each}
              </tr>
              <tr>
                <th scope="row">Total time</th>
                {#each graph.sides as side (side.label)}<td
                    >{side.total_time}</td
                  >{/each}
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
        {#if error}<p class="usage-note" role="alert">{error}</p>{/if}
      </section>
      <hr />
      <button class="usage-link" onclick={openProviders}
        >Settings · Providers</button
      >
    </div>
  </div>
</dialog>
