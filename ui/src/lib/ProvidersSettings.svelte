<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import SettingsNav from './SettingsNav.svelte';
  import {
    providerCommands,
    onProviders,
    providerIcon,
    providerMark,
  } from './ipc';
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
  // The Add an ACP agent form: open, and what you typed so far.
  let adding = $state(false);
  let agentName = $state('');
  let agentCommand = $state('');

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
  const setCommand = (provider: ProviderKind, command: string | null) =>
    run(providerCommands.setCommand(provider, command));
  const removeAgent = (provider: ProviderKind) =>
    run(providerCommands.removeAgent(provider));

  function addAgent() {
    error = '';
    providerCommands
      .addAgent(agentName, agentCommand)
      .then((next) => {
        view = next;
        adding = false;
        agentName = '';
        agentCommand = '';
      })
      .catch(report);
  }

  // Escape puts back the saved command and leaves the field without saving.
  function commandKey(event: KeyboardEvent, saved: string) {
    const field = event.currentTarget as HTMLInputElement;
    if (event.key === 'Escape') {
      field.value = saved;
      field.blur();
    } else if (event.key === 'Enter') {
      field.blur();
    }
  }

  function saveCommand(provider: ProviderCardView, text: string) {
    if (text.trim() === provider.command_line) return;
    setCommand(provider.provider, text);
  }

  function cancelAdding() {
    adding = false;
    agentName = '';
    agentCommand = '';
  }

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
  <div class="pv" data-provider={providerMark(provider.provider)}>
    <span class="pv-mark"><Icon name={providerIcon(provider.provider)} /></span>
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
      {#if provider.custom}<button
          class="worktree-button mini"
          title="Remove this agent. Its threads keep their history."
          onclick={() => removeAgent(provider.provider)}>Remove</button
        >{/if}
    </div>
    <div class="pv-command">
      ACP agent <label class="pv-command-input"
        ><input
          aria-label={`${provider.name} ACP agent command`}
          value={provider.command_line}
          spellcheck="false"
          onkeydown={(event) => commandKey(event, provider.command_line)}
          onchange={(event) => saveCommand(provider, event.currentTarget.value)}
        /></label
      ><span>· stdio</span>
      {#if provider.command_edited}<button
          class="worktree-button mini"
          title="Return to the built-in command"
          onclick={() => setCommand(provider.provider, null)}>Reset</button
        >{/if}
    </div>
    {#if provider.limit}<div class="pv-limit">
        <span class="pv-bar"
          ><i style={`width: ${provider.limit.percent_used}%`}></i></span
        >{provider.limit.label}
      </div>{/if}
    <div class="pv-command">
      <label class="pv-command-input"
        >Max agents at once
        <input
          type="number"
          min="1"
          max="4294967295"
          step="1"
          aria-label={provider.name + ' Max agents at once'}
          placeholder="Off"
          value={provider.max_agents ?? ''}
          onkeydown={(event) =>
            commandKey(event, String(provider.max_agents ?? ''))}
          onchange={(event) => {
            const field = event.currentTarget;
            if (field.value !== '' && !field.reportValidity()) return;
            edit({
              MaxAgents: {
                provider: provider.provider,
                maximum: field.value === '' ? null : field.valueAsNumber,
              },
            });
          }}
        />
      </label>
      <span>Off when empty</span>
    </div>
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
        ><span
          class="pv-model-head"
          data-provider={providerMark(model.provider)}
          ><Icon name={providerIcon(model.provider)} />{model.model}</span
        ><span class="pv-model-note">{model.note}</span></button
      >{/each}
    {#each choices.sign_in as provider (provider.provider)}<button
        class="pv-model out"
        disabled={!provider.can_sign_in}
        onclick={() => signIn(provider.provider)}
        ><span
          class="pv-model-head"
          data-provider={providerMark(provider.provider)}
          ><Icon name={providerIcon(provider.provider)} />{provider.name}</span
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
          {#if adding}<form
              class="pv-add"
              onsubmit={(event) => {
                event.preventDefault();
                addAgent();
              }}
            >
              <p class="worktree-description">
                Any agent that speaks ACP over stdio, as in Zed. It gets its own
                card, its models are read over ACP, and it runs unrestricted
                with every tool. It inherits your environment, so there is no
                field for a variable or a key.
              </p>
              <div class="pv-add-row">
                <label class="worktree-input"
                  ><span>Name</span><input
                    aria-label="Agent name"
                    bind:value={agentName}
                    spellcheck="false"
                  /></label
                >
                <label class="worktree-input pv-add-command"
                  ><span>Command</span><input
                    aria-label="Agent command and arguments"
                    placeholder="gemini --experimental-acp"
                    bind:value={agentCommand}
                    spellcheck="false"
                  /></label
                >
              </div>
              <div class="pv-add-row">
                <button class="worktree-button" type="submit">Add agent</button
                ><button
                  class="worktree-button mini"
                  type="button"
                  onclick={cancelAdding}>Cancel</button
                >
              </div>
            </form>{:else}<button
              class="worktree-button pv-add-open"
              onclick={() => (adding = true)}>Add an ACP agent</button
            >{/if}
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
