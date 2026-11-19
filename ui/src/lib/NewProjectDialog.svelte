<script lang="ts">
  import { onMount } from 'svelte';
  import { modal } from './motion';
  import Icon from './Icon.svelte';
  import ProviderSignInCards from './ProviderSignInCards.svelte';
  import {
    collapseConnectionCommands,
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
    ProjectMirror,
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
  const mirrors = ['none', 'collapse', 'github'] as const;
  const mirrorLabels = { none: 'None', collapse: 'Collapse', github: 'GitHub' };
  let mirrorKind = $state<(typeof mirrors)[number]>('none');
  // The connected Collapse account (#427). Collapse is the default mirror once
  // connected, and a blank name creates a repository named after the folder.
  let collapseAccount = $state<string | null>(null);
  let mirrorChosen = false;
  let folder = $state('');
  let repository = $state('');
  let effort = $state<EffortLevel>('high');
  let speed = $state<SpeedMode>('regular');
  let brief = $state('');
  let creating = $state(false);
  let error = $state('');
  let handledSubmits = 0;

  onMount(() => {
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
    void newProjectCommands
      .defaults()
      .then((loaded) => {
        defaults = loaded;
        effort = loaded.effort;
        speed = loaded.speed;
      })
      .catch(report);
    void collapseConnectionCommands
      .read()
      .then((connection) => {
        if (!active) return;
        collapseAccount = connection.account ?? null;
        if (connection.account && !mirrorChosen) mirrorKind = 'collapse';
      })
      // An unreadable connection only loses the default; Create reports it.
      .catch(() => (collapseAccount = null));
    return () => {
      active = false;
      void listening.then((stop) => stop()).catch(report);
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
      if (picked !== null) folder = picked;
    } catch (reason) {
      report(reason);
    }
  }

  function location(): ProjectLocationRequest {
    let mirror: ProjectMirror = 'None';
    if (mirrorKind === 'github') mirror = { GitHub: { repository } };
    if (mirrorKind === 'collapse') mirror = { Collapse: { repository } };
    return { RosenFolder: { path: folder, mirror } };
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
  transition:modal|global={{ focus: () => briefInput.focus() }}
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
        <div class="row2">
          <label class="inp">
            <input
              aria-label="Project folder"
              placeholder={defaults?.folder ?? ''}
              spellcheck="false"
              bind:value={folder}
            />
          </label>
          <button class="btn" onclick={browse}>Browse…</button>
        </div>
      </fieldset>
      <fieldset class="field">
        <legend class="label">Mirror</legend>
        <div class="seg" role="group" aria-label="Mirror">
          {#each mirrors as choice (choice)}
            <button
              class:on={mirrorKind === choice}
              aria-pressed={mirrorKind === choice}
              onclick={() => {
                mirrorKind = choice;
                mirrorChosen = true;
                repository = '';
              }}>{mirrorLabels[choice]}</button
            >
          {/each}
        </div>
        {#if mirrorKind !== 'none'}
          <label class="inp">
            <span class="pre"
              >{mirrorKind === 'github'
                ? 'github.com/'
                : collapseAccount
                  ? `collapse.sh/${collapseAccount}/`
                  : 'api.collapse.sh/'}</span
            >
            <input
              aria-label="Mirror repository"
              spellcheck="false"
              placeholder={mirrorKind === 'collapse' && collapseAccount
                ? 'new repository named after the folder'
                : 'owner/repository'}
              bind:value={repository}
            />
          </label>
          {#if mirrorKind === 'collapse' && !collapseAccount}<p class="np-note">
              Connect Collapse in Settings · Agents to create the repository
              here, or name one whose push token git already has.
            </p>{/if}
        {/if}
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
