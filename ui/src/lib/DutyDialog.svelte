<script lang="ts">
  import { onMount } from 'svelte';
  import { modal } from './motion';
  import Icon from './Icon.svelte';
  import ProviderSignInCards from './ProviderSignInCards.svelte';
  import { dutyCommands, providerIcon } from './ipc';
  import type {
    DutyDialogDefaults,
    EffortLevel,
    LayoutAction,
    LayoutSnapshot,
    ScoutCadence,
    ScoutFocusArea,
    ScoutRuleSettings,
    SpeedMode,
    SwarmModelCard,
  } from './ipc';
  import './theme/new-project.css';

  let {
    send,
    submitRequests,
    onResult,
  }: {
    send: (action: LayoutAction) => void;
    submitRequests: number;
    onResult: (snapshot: LayoutSnapshot) => void;
  } = $props();

  const focusOptions: [ScoutFocusArea, string][] = [
    ['bugs', 'Bugs'],
    ['stress', 'Stress'],
    ['proof_gaps', 'Proof gaps'],
    ['spec_drift', 'Spec drift'],
    ['performance', 'Performance'],
    ['security', 'Security'],
    ['dead_code', 'Dead code'],
    ['ux', 'UX'],
  ];

  const efforts: [EffortLevel, string][] = [
    ['low', 'Low'],
    ['medium', 'Medium'],
    ['high', 'High'],
    ['max', 'Max'],
  ];

  const cadences: [ScoutCadence, string][] = [
    ['once', 'Once'],
    ['every_thirty_minutes', '30m minimum'],
    ['after_every_land', 'After every land'],
  ];

  let dialog: HTMLDialogElement;
  let defaults = $state<DutyDialogDefaults | null>(null);
  let agentCount = $state(1);
  // A card is one model of one provider; a provider lists several, so the pair is the key.
  // A custom agent's provider is an object, so it is compared through its JSON.
  const cardKey = (card: SwarmModelCard) =>
    `${JSON.stringify(card.provider)}:${card.model_name}`;
  let chosenKey = $state<string | null>(null);
  let effort = $state<EffortLevel | null>(null);
  let speed = $state<SpeedMode>('regular');
  let scoutRules = $state<ScoutRuleSettings>({
    focus_areas: ['bugs', 'stress', 'proof_gaps', 'spec_drift'],
    assume_every_part_fails: true,
    run_stress_against_models: true,
    label_issues_for_dispatch: false,
    cadence: 'every_thirty_minutes',
  });
  let repeatUnchanged = $state(false);
  let submitting = $state(false);
  let error = $state('');
  let handledSubmits = 0;

  onMount(() => {
    handledSubmits = submitRequests;
    void dutyCommands
      .defaults()
      .then((loaded) => {
        defaults = loaded;
        const initial = loaded.models.find((card) => card.selected);
        chosenKey = initial ? cardKey(initial) : null;
        speed = loaded.speed;
        scoutRules = loaded.scout_rules;
      })
      .catch(report);
  });

  $effect(() => {
    if (submitRequests !== handledSubmits) {
      handledSubmits = submitRequests;
      void submit();
    }
  });

  function report(reason: unknown) {
    error = reason instanceof Error ? reason.message : String(reason);
  }

  function dismiss() {
    if (!submitting) send({ Request: { Navigation: 'Dismiss' } });
  }

  function stepCount(delta: number) {
    agentCount = Math.max(1, Math.min(4, agentCount + delta));
  }

  function toggleFocus(area: ScoutFocusArea) {
    if (scoutRules.focus_areas.includes(area)) {
      scoutRules.focus_areas = scoutRules.focus_areas.filter(
        (existing) => existing !== area,
      );
    } else {
      scoutRules.focus_areas = [...scoutRules.focus_areas, area];
    }
  }

  const reviewSwarm = $derived(defaults?.review_swarm ?? false);
  const reviewWindows: [ScoutCadence, string][] = [
    ['every_thirty_minutes', '30m'],
    ['after_every_land', '1m'],
  ];
  const offeredCadences = $derived(reviewSwarm ? reviewWindows : cadences);
  const selectedCadence = $derived(
    reviewSwarm && scoutRules.cadence === 'once'
      ? 'every_thirty_minutes'
      : scoutRules.cadence,
  );

  const chosenModel = $derived(
    defaults?.models.find((model) => cardKey(model) === chosenKey) ?? null,
  );

  async function submit() {
    if (submitting) return;
    submitting = true;
    error = '';
    try {
      const request = {
        role: 'scouts' as const,
        agent_count: agentCount,
        provider: chosenModel?.provider ?? defaults?.provider ?? null,
        model_name: chosenModel?.model_name ?? defaults?.model_name ?? null,
        effort,
        speed,
        scout_rules: { ...scoutRules, cadence: selectedCadence },
        repeat_unchanged:
          !reviewSwarm && selectedCadence !== 'once' && repeatUnchanged,
      };
      const snapshot = defaults?.scouts_exists
        ? await dutyCommands.addAgents(request)
        : await dutyCommands.start(request);
      onResult(snapshot);
    } catch (reason) {
      report(reason);
    } finally {
      submitting = false;
    }
  }
</script>

<dialog
  bind:this={dialog}
  transition:modal|global
  class="duty-dialog"
  aria-labelledby="duty-dialog-title"
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
      <h2 id="duty-dialog-title">{reviewSwarm ? 'Review swarm' : 'Scouts'}</h2>
      <span class="label">Esc to cancel</span>
    </div>
    <div class="np-body">
      <div class="field">
        <div class="row2 spread">
          <span class="label">Agents</span>
          <div class="stepper" role="group" aria-label="Agent count">
            <button
              type="button"
              aria-label="Decrease agents"
              onclick={() => stepCount(-1)}>−</button
            >
            <span>{agentCount}</span>
            <button
              type="button"
              aria-label="Increase agents"
              onclick={() => stepCount(1)}>+</button
            >
          </div>
        </div>
        <div class="hint"></div>
      </div>

      <div class="field">
        <div class="row2 spread">
          <span class="label" id="duty-model-label">Model</span>
          <div class="row2">
            <div class="seg sm" role="group" aria-label="Effort">
              <button
                type="button"
                class:on={effort === null}
                aria-pressed={effort === null}
                onclick={() => (effort = null)}>Role default</button
              >
              {#each efforts as [value, label] (value)}<button
                  type="button"
                  class:on={effort === value}
                  aria-pressed={effort === value}
                  onclick={() => (effort = value)}>{label}</button
                >{/each}
            </div>
            <div class="seg sm" role="group" aria-label="Speed">
              <button
                type="button"
                class:on={speed === 'regular'}
                aria-pressed={speed === 'regular'}
                onclick={() => (speed = 'regular')}>Regular</button
              ><button
                type="button"
                class:on={speed === 'fast'}
                aria-pressed={speed === 'fast'}
                onclick={() => (speed = 'fast')}
                ><Icon name="bolt" />Fast</button
              >
            </div>
          </div>
        </div>
        <div class="models" role="group" aria-labelledby="duty-model-label">
          {#each defaults?.models ?? [] as model (cardKey(model))}
            <button
              type="button"
              class="model-card"
              class:on={cardKey(model) === chosenKey}
              aria-pressed={cardKey(model) === chosenKey}
              onclick={() => (chosenKey = cardKey(model))}
            >
              <span class="mh">
                <Icon name={providerIcon(model.provider)} />
                {model.model_name}
              </span>
              <span class="md">
                {model.provider_name}{model.selected &&
                !defaults?.model_configured
                  ? ' · default'
                  : ''}
              </span>
            </button>
          {/each}
          <ProviderSignInCards {send} {report} />
        </div>
      </div>

      <div class="field">
        <span class="label">Look for</span>
        <div class="focus-chips" role="group" aria-label="Look for">
          {#each focusOptions as [area, label] (area)}
            <button
              type="button"
              class:on={scoutRules.focus_areas.includes(area)}
              aria-pressed={scoutRules.focus_areas.includes(area)}
              onclick={() => toggleFocus(area)}>{label}</button
            >
          {/each}
        </div>
        <div class="checks sub-row">
          <label class="check locked">
            <input type="checkbox" checked disabled />
            Never file duplicates (checks open and closed issues)
            <span class="lockt"><Icon name="lock" /> always on</span>
          </label>
          <label class="check locked">
            <input type="checkbox" checked disabled />
            No issue without evidence: a failing test or stress script
            <span class="lockt"><Icon name="lock" /> always on</span>
          </label>
          <label class="check">
            <input
              type="checkbox"
              bind:checked={scoutRules.assume_every_part_fails}
            />
            Assume every part fails. Write tests and stress scripts to prove it
          </label>
          <label class="check">
            <input
              type="checkbox"
              bind:checked={scoutRules.run_stress_against_models}
            />
            Run stress inputs against the Lean reference models
          </label>
          <label class="check">
            <input
              type="checkbox"
              bind:checked={scoutRules.label_issues_for_dispatch}
            />
            Label issues so they can be dispatched straight to agents
          </label>
          {#if !reviewSwarm && scoutRules.cadence !== 'once'}
            <label class="check">
              <input type="checkbox" bind:checked={repeatUnchanged} />
              Sweep an unchanged base every 30m with fresh generated inputs
            </label>
          {/if}
        </div>
        <div class="row2 spread sub-row">
          <span class="label">{reviewSwarm ? 'Review window' : 'Sweep'}</span>
          <div
            class="seg sm"
            role="group"
            aria-label={reviewSwarm ? 'Review window' : 'Sweep'}
          >
            {#each offeredCadences as [value, label] (value)}
              <button
                type="button"
                class:on={selectedCadence === value}
                aria-pressed={selectedCadence === value}
                onclick={() => (scoutRules.cadence = value)}>{label}</button
              >
            {/each}
          </div>
        </div>
      </div>
    </div>
    <div class="np-f">
      {#if error}<p class="np-error" role="alert">{error}</p>{/if}
      <span class="spacer"></span>
      <button type="button" class="btn" disabled={submitting} onclick={dismiss}>
        Cancel
      </button>
      <button
        type="button"
        class="btn solid"
        disabled={submitting}
        onclick={submit}
      >
        {defaults?.scouts_exists ? 'Add agents' : 'Start'}
        <kbd>Ctrl</kbd><kbd>↵</kbd>
      </button>
    </div>
  </div>
</dialog>
