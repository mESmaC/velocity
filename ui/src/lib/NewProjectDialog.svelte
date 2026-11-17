<script lang="ts">
  import { onMount } from 'svelte';
  import { pop } from './motion';
  import Icon from './Icon.svelte';
  import ProviderSignInCards from './ProviderSignInCards.svelte';
  import {
    newProjectCommands,
    providerCommands,
    onProviders,
    providerIcon,
  } from './ipc';
  import type {
    EffortLevel,
    LayoutAction,
    ModelCardView,
    NewProjectDefaults,
    ProjectLocationRequest,
    RepositoryVisibility,
    SpeedMode,
  } from './ipc';
  import './theme/new-project.css';

  let {
    send,
    submitRequests,
  }: { send: (action: LayoutAction) => void; submitRequests: number } =
    $props();

  const efforts: [EffortLevel, string][] = [
    ['low', 'Low'],
    ['medium', 'Medium'],
    ['high', 'High'],
    ['max', 'Max'],
  ];
  const speeds: [SpeedMode, string][] = [
    ['regular', 'Regular'],
    ['fast', 'Fast'],
  ];
  let dialog: HTMLDialogElement;
  let briefInput: HTMLElement;
  let defaults = $state<NewProjectDefaults | null>(null);
  let models = $state<ModelCardView[]>([]);
  let modelName = $state<string | null>(null);
  let where = $state<'local' | 'github'>('local');
  let githubKind = $state<'new' | 'existing'>('new');
  let folder = $state('');
  let createIfNeeded = $state(true);
  let repository = $state('');
  let visibility = $state<RepositoryVisibility>('Private');
  let cloneTo = $state('');
  let effort = $state<EffortLevel>('high');
  let speed = $state<SpeedMode>('regular');
  let brief = $state('');
  let creating = $state(false);
  let error = $state('');
  let handledSubmits = 0;

  onMount(() => {
    const previous = document.activeElement;
    let active = true;
    const listening = onProviders((event) => {
      if (active) models = event.payload.view.defaults.cards;
    });
    void listening.catch(report);
    void providerCommands
      .list()
      .then((view) => {
        if (active) {
          models = view.defaults.cards;
          modelName ??= models.find((model) => model.selected)?.model ?? null;
        }
      })
      .catch(report);
    handledSubmits = submitRequests;
    dialog.showModal();
    briefInput.focus();
    void newProjectCommands
      .defaults()
      .then((loaded) => {
        defaults = loaded;
        folder = loaded.folder;
        cloneTo = loaded.clone_to;
        effort = loaded.effort;
        speed = loaded.speed;
      })
      .catch(report);
    return () => {
      active = false;
      void listening.then((stop) => stop()).catch(report);
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus();
    };
  });

  // Ctrl+Enter is decided in Rust (§5) and arrives as a Submit request.
  $effect(() => {
    if (submitRequests !== handledSubmits) {
      handledSubmits = submitRequests;
      void create();
    }
  });

  function report(reason: unknown) {
    error = reason instanceof Error ? reason.message : String(reason);
  }

  function dismiss() {
    if (!creating) send({ Request: { Navigation: 'Dismiss' } });
  }

  async function browse() {
    try {
      const picked = await newProjectCommands.pickFolder();
      if (picked !== null) {
        if (where === 'local') folder = picked;
        else cloneTo = picked;
      }
    } catch (reason) {
      report(reason);
    }
  }

  function location(): ProjectLocationRequest {
    if (where === 'local')
      return {
        LocalFolder: { path: folder, create_if_needed: createIfNeeded },
      };
    if (githubKind === 'new')
      return {
        GitHubNew: { repository, visibility, clone_to: cloneTo },
      };
    return { GitHubExisting: { repository, clone_to: cloneTo } };
  }

  async function create() {
    if (creating) return;
    creating = true;
    error = '';
    try {
      await newProjectCommands.create({
        location: location(),
        model_name: modelName ?? defaults?.model_name ?? null,
        effort,
        speed,
        brief,
      });
    } catch (reason) {
      report(reason);
    } finally {
      creating = false;
    }
  }
</script>

<dialog
  bind:this={dialog}
  in:pop|global
  class="new-project-dialog"
  aria-labelledby="new-project-title"
  oncancel={(event) => {
    event.preventDefault();
    dismiss();
  }}
  onclick={(event) => {
    if (event.target === dialog) dismiss();
  }}
>
  <div class="np">
    <div class="np-h">
      <h2 id="new-project-title">New project</h2>
      <span class="label">Esc to cancel</span>
    </div>
    <div class="np-body">
      <fieldset class="field">
        <legend class="label">Location</legend>
        <div class="seg" role="group" aria-label="Location">
          <button
            class:on={where === 'local'}
            aria-pressed={where === 'local'}
            onclick={() => (where = 'local')}
            ><Icon name="localFolder" />Local folder</button
          >
          <button
            class:on={where === 'github'}
            aria-pressed={where === 'github'}
            onclick={() => (where = 'github')}
            ><Icon name="github" />GitHub</button
          >
        </div>
        {#if where === 'local'}<div class="where">
            <div class="row2">
              <label class="inp"
                ><input
                  aria-label="Project folder"
                  spellcheck="false"
                  bind:value={folder}
                /></label
              >
              <button class="btn" onclick={browse}>Browse…</button>
            </div>
            <label class="check"
              ><input type="checkbox" bind:checked={createIfNeeded} /> Create
              the folder and run <span class="mono">git init</span> if needed</label
            >
          </div>{:else}<div class="where">
            <div class="seg sm" role="group" aria-label="Repository">
              <button
                class:on={githubKind === 'new'}
                aria-pressed={githubKind === 'new'}
                onclick={() => (githubKind = 'new')}>New repo</button
              >
              <button
                class:on={githubKind === 'existing'}
                aria-pressed={githubKind === 'existing'}
                onclick={() => (githubKind = 'existing')}>Existing repo</button
              >
            </div>
            <div class="row2">
              <label class="inp"
                ><span class="pre">github.com/</span><input
                  aria-label="Repository, owner/name"
                  spellcheck="false"
                  placeholder="owner/name"
                  bind:value={repository}
                /></label
              >
              {#if githubKind === 'new'}<div
                  class="seg sm"
                  role="group"
                  aria-label="Visibility"
                >
                  <button
                    class:on={visibility === 'Private'}
                    aria-pressed={visibility === 'Private'}
                    onclick={() => (visibility = 'Private')}>Private</button
                  >
                  <button
                    class:on={visibility === 'Public'}
                    aria-pressed={visibility === 'Public'}
                    onclick={() => (visibility = 'Public')}>Public</button
                  >
                </div>{/if}
            </div>
            <div class="row2">
              <label class="inp"
                ><span class="pre">clone to</span><input
                  aria-label="Clone to"
                  spellcheck="false"
                  bind:value={cloneTo}
                /></label
              >
              <button class="btn" onclick={browse}>Browse…</button>
            </div>
          </div>{/if}
      </fieldset>
      <div class="field">
        <div class="row2 spread">
          <span class="label" id="new-project-model-label"
            >Model for the interview</span
          >
          <div class="row2">
            <div class="seg sm" role="group" aria-label="Effort">
              {#each efforts as [value, label] (value)}<button
                  class:on={effort === value}
                  aria-pressed={effort === value}
                  onclick={() => (effort = value)}>{label}</button
                >{/each}
            </div>
            <div class="seg sm" role="group" aria-label="Speed">
              {#each speeds as [value, label] (value)}<button
                  class:on={speed === value}
                  aria-pressed={speed === value}
                  onclick={() => (speed = value)}>{label}</button
                >{/each}
            </div>
          </div>
        </div>
        <div
          class="models"
          role="group"
          aria-labelledby="new-project-model-label"
        >
          {#each models as model (`${model.provider}:${model.model}`)}
            <button
              type="button"
              class="model-card"
              class:on={modelName === model.model}
              aria-pressed={modelName === model.model}
              title={`${model.model} · ${model.note}`}
              onclick={() => (modelName = model.model)}
            >
              <span class="mh"
                ><Icon name={providerIcon(model.provider)} />{model.model}</span
              >
              <span class="md">{model.note}</span>
            </button>
          {/each}
          {#if defaults && models.length === 0}<p class="np-note">
              No provider is signed in yet. The interview uses the model you
              choose in Settings · Providers once one is.
            </p>{/if}
          <ProviderSignInCards {send} {report} />
        </div>
      </div>
      <label class="field">
        <span class="label">What are you building?</span>
        <textarea
          bind:this={briefInput}
          class="brief"
          spellcheck="false"
          bind:value={brief}></textarea>
      </label>
      <p class="np-note">
        <Icon name="info" />
        <span
          >The agent looks through the location first, then interviews you one
          question at a time until nothing is ambiguous. Your answers become
          SPEC.md and the project Task List.</span
        >
      </p>
    </div>
    <div class="np-f">
      {#if error}<p class="np-error" role="alert">{error}</p>{/if}
      <span class="spacer"></span>
      <button class="btn" disabled={creating} onclick={dismiss}>Cancel</button>
      <button class="btn solid" disabled={creating} onclick={create}
        >Create & start interview <kbd>Ctrl</kbd><kbd>↵</kbd></button
      >
    </div>
  </div>
</dialog>
