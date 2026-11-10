<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import SettingsNav from './SettingsNav.svelte';
  import { providerCommands, onProviders } from './ipc';
  import type {
    EffortLevel,
    LayoutAction,
    ModelChoiceView,
    ProviderCardView,
    ProviderEdit,
    ProviderKind,
    ProvidersView,
    SpeedMode,
  } from './ipc';
  import './theme/worktrees.css';
  import './theme/providers.css';

  let { send }: { send: (action: LayoutAction) => void } = $props();
  let view = $state<ProvidersView | null>(null);
  let error = $state('');

  const efforts: EffortLevel[] = ['low', 'medium', 'high', 'max'];
  const effortNames: Record<EffortLevel, string> = {
    low: 'Low',
    medium: 'Medium',
    high: 'High',
    max: 'Max',
  };
  const speeds: SpeedMode[] = ['regular', 'fast'];
  const providerIcons: Record<ProviderKind, string> = {
    claude: 'claude',
    codex: 'codex',
    antigravity: 'antigravity',
    kimi: 'kimi',
  };

  function report(reason: unknown) {
    error = reason instanceof Error ? reason.message : String(reason);
  }

  function run(request: Promise<ProvidersView>) {
    error = '';
    request.then((next) => (view = next)).catch(report);
  }

  const edit = (change: ProviderEdit) => run(providerCommands.edit(change));
  const signIn = (provider: ProviderKind) =>
    run(providerCommands.signIn(provider));
  const signOut = (provider: ProviderKind) =>
    run(providerCommands.signOut(provider));

  onMount(() => {
    let active = true;
    let stop: (() => void) | undefined;
    async function start() {
      const unlisten = await onProviders((event) => {
        if (active) view = event.payload.view;
      });
      if (!active) {
        unlisten();
        return;
      }
      stop = unlisten;
      const snapshot = await providerCommands.list();
      if (active) view = snapshot;
    }
    void start().catch(report);
    return () => {
      active = false;
      stop?.();
    };
  });
</script>

{#snippet card(provider: ProviderCardView)}
  <div class="pv" data-provider={provider.provider}>
    <span class="pv-mark"><Icon name={providerIcons[provider.provider]} /></span
    >
    <div class="pv-name">{provider.name} <span>{provider.runs}</span></div>
    <div class="pv-status">
      <i
        class:off={provider.phase !== 'SignedIn' &&
          provider.phase !== 'Waiting'}
        class:wait={provider.phase === 'Waiting' ||
          provider.phase === 'Checking'}
      ></i>{provider.status}
    </div>
    <div class="pv-actions">
      {#if provider.phase === 'SignedIn'}<button
          class="worktree-button mini"
          onclick={() => signOut(provider.provider)}>Sign out</button
        >{:else if provider.phase === 'Waiting'}<button
          class="worktree-button mini"
          onclick={() => signOut(provider.provider)}>Cancel</button
        >{:else if provider.phase !== 'Checking' && provider.phase !== 'NotInstalled'}<button
          class="worktree-button"
          disabled={!provider.can_sign_in}
          title={provider.can_sign_in
            ? 'OAuth in your browser'
            : 'Its agent offers no sign-in Velocity can use'}
          onclick={() => signIn(provider.provider)}
          >{provider.sign_in_label}</button
        >{/if}
    </div>
    <div class="pv-command">ACP agent <code>{provider.command}</code></div>
    {#if provider.limit}<div class="pv-limit">
        <span class="pv-bar"
          ><i style={`width: ${provider.limit.percent_used}%`}></i></span
        >{provider.limit.label}
      </div>{/if}
    {#if provider.models.length}<div class="pv-models">
        <span>Models</span>
        {#each provider.models as chip (chip.model)}<button
            class="pv-chip"
            class:on={chip.shown}
            aria-pressed={chip.shown}
            title={chip.shown ? 'Hide from the pickers' : 'Show in the pickers'}
            onclick={() =>
              edit({
                ShowModel: {
                  provider: provider.provider,
                  model: chip.model,
                  shown: !chip.shown,
                },
              })}>{chip.model}</button
          >{/each}
      </div>{/if}
  </div>
{/snippet}

{#snippet choice(
  choices: ModelChoiceView,
  pick: (provider: ProviderKind, model: string) => void,
  effort: (level: EffortLevel) => void,
  speed: (mode: SpeedMode) => void,
)}
  <div class="pv-cards">
    {#each choices.cards as model (`${model.provider}:${model.model}`)}<button
        class="pv-model"
        class:on={model.selected}
        aria-pressed={model.selected}
        onclick={() => pick(model.provider, model.model)}
        ><span class="pv-model-head" data-provider={model.provider}
          ><Icon name={providerIcons[model.provider]} />{model.model}</span
        ><span class="pv-model-note">{model.note}</span></button
      >{/each}
    {#each choices.sign_in as provider (provider.provider)}<button
        class="pv-model out"
        disabled={!provider.can_sign_in}
        onclick={() => signIn(provider.provider)}
        ><span class="pv-model-head" data-provider={provider.provider}
          ><Icon name={providerIcons[provider.provider]} />{provider.name}</span
        ><span class="pv-model-note">{provider.note}</span></button
      >{/each}
  </div>
  <div class="pv-row">
    <span>Effort</span>
    <span class="pv-seg">
      {#each efforts as level (level)}<button
          class:on={choices.effort === level}
          onclick={() => effort(level)}>{effortNames[level]}</button
        >{/each}
    </span>
  </div>
  <div class="pv-row">
    <span>Speed</span>
    <span
      class="pv-seg"
      class:dis={!choices.speed_available}
      title={choices.speed_note ?? undefined}
    >
      {#each speeds as mode (mode)}<button
          class:on={(choices.speed_available ? choices.speed : 'regular') ===
            mode}
          disabled={!choices.speed_available}
          onclick={() => speed(mode)}
          >{#if mode === 'fast'}<Icon
              name="bolt"
            />Fast{:else}Regular{/if}</button
        >{/each}
    </span>
  </div>
{/snippet}

<section class="worktree-settings" aria-label="Providers settings">
  <header class="worktree-settings-head">
    <h1>Settings</h1>
    <span>applies to every project</span>
  </header>
  <div class="worktree-settings-body">
    <SettingsNav
      current="providers"
      providersLabel={view?.signed_in_label ?? ''}
      {send}
    />
    <div class="worktree-settings-main">
      {#if error}<p class="pv-error" role="alert">{error}</p>{/if}
      {#if view}
        <section>
          <h2>Providers</h2>
          <p class="worktree-description">
            Agents run on your own subscriptions. Each provider signs in with
            OAuth in your browser, the way T3 Code does. Velocity talks to every
            provider's agent over ACP (Agent Client Protocol), the same protocol
            T3 Code and Zed use, and never asks for or stores an API key. Tokens
            stay in each provider's own credential store.
          </p>
          <div class="pv-list">
            {#each view.cards as provider (provider.provider)}{@render card(
                provider,
              )}{/each}
          </div>
        </section>
        <section>
          <h2>Default for new threads</h2>
          <p class="worktree-description">
            Spec interviews, duty agents and swarms choose their models in their
            own dialogs. A thread can switch models from the composer.
          </p>
          {@render choice(
            view.defaults,
            (provider, model) => edit({ DefaultModel: { provider, model } }),
            (level) => edit({ DefaultEffort: level }),
            (mode) => edit({ DefaultSpeed: mode }),
          )}
        </section>
        <section>
          <h2>Thread titles</h2>
          <p class="worktree-description">
            A small, fast model names each thread from your first message, and
            the branch is named from the title. It runs once per thread, so pick
            something quick.
          </p>
          {@render choice(
            view.titles,
            (provider, model) => edit({ TitleModel: { provider, model } }),
            (level) => edit({ TitleEffort: level }),
            (mode) => edit({ TitleSpeed: mode }),
          )}
          <p class="pv-title-note" class:warn={view.title_warning}>
            <span>{view.title_note}</span>
            {#if view.title_is_default}{#if !view.title_warning}<span
                  >· the default</span
                >{/if}{:else}<span>·</span>
              <button
                class="worktree-button mini"
                onclick={() => edit('TitleReset')}
                >Reset to GPT-6 Luna · Low · Fast</button
              >{/if}
          </p>
        </section>
      {:else}
        <p class="worktree-description">Reading providers…</p>
      {/if}
    </div>
  </div>
</section>
