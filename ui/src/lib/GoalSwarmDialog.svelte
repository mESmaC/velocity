<script lang="ts">
  import { onMount } from 'svelte';
  import { modal } from './motion';
  import Icon from './Icon.svelte';
  import ProviderSignInCards from './ProviderSignInCards.svelte';
  import { goalSwarmCommands, providerIcon } from './ipc';
  import type {
    EffortLevel,
    GoalDialogRequest,
    GoalMixEdit,
    GoalSwarmChoices,
    GoalSwarmDialog,
    LayoutAction,
    LayoutSnapshot,
    WorkSources,
  } from './ipc';
  import './theme/new-project.css';
  import './theme/goal-swarm.css';

  let {
    request,
    send,
    submitRequests,
    onResult,
  }: {
    request: GoalDialogRequest;
    send: (action: LayoutAction) => void;
    submitRequests: number;
    onResult: (snapshot: LayoutSnapshot) => void;
  } = $props();

  const efforts: [EffortLevel, string][] = [
    ['low', 'Low'],
    ['medium', 'Med'],
    ['high', 'High'],
    ['max', 'Max'],
  ];

  const workSources: [keyof WorkSources, string][] = [
    ['tasks', 'Task List'],
    ['issues', 'Issues'],
    ['sections', 'Unbuilt spec sections'],
  ];

  let dialogElement: HTMLDialogElement;
  let view = $state<GoalSwarmDialog | null>(null);
  // `null` asks Rust for the defaults; afterwards it echoes the choices it drew.
  let choices: GoalSwarmChoices | null = null;
  let prompt = $state('');
  let keepGoing = $state(false);
  let submitting = $state(false);
  let error = $state('');
  let handledSubmits = 0;
  // Edits are applied in Rust one after another, each on the choices the last one returned.
  let queue: Promise<void> = Promise.resolve();

  onMount(() => {
    prompt = request.prompt;
    handledSubmits = submitRequests;
    draw();
    // The pool is read after the saved source choices; the dialog is usable meanwhile.
    refreshPool();
  });

  function report(reason: unknown) {
    error = reason instanceof Error ? reason.message : String(reason);
  }

  function draw(edit: GoalMixEdit | null = null) {
    queue = queue
      .then(() =>
        goalSwarmCommands.dialog({
          choices,
          edit,
          into: request.into === null ? null : Number(request.into),
        }),
      )
      .then((next) => {
        view = next;
        choices = next.choices;
        error = '';
      })
      .catch(report);
    if (edit?.kind === 'set_sources') refreshPool();
  }

  function setSource(source: keyof WorkSources, checked: boolean) {
    // Read the latest choices when this edit reaches the queue, so quick toggles compose.
    queue = queue
      .then(async () => {
        if (!view) return;
        const next = await goalSwarmCommands.dialog({
          choices,
          edit: {
            kind: 'set_sources',
            sources: { ...view.choices.sources, [source]: checked },
          },
          into: request.into === null ? null : Number(request.into),
        });
        view = next;
        choices = next.choices;
        error = '';
      })
      .catch(report);
    refreshPool();
  }

  function refreshPool() {
    queue = queue
      .then(() => goalSwarmCommands.readPool(view?.choices.sources ?? null))
      .then(() =>
        goalSwarmCommands.dialog({
          choices,
          edit: null,
          into: request.into === null ? null : Number(request.into),
        }),
      )
      .then((next) => {
        view = next;
        choices = next.choices;
        error = '';
      })
      .catch(report);
  }

  $effect(() => {
    if (submitRequests !== handledSubmits) {
      handledSubmits = submitRequests;
      void submit();
    }
  });

  function dismiss() {
    if (!submitting) send({ Request: { Navigation: 'Dismiss' } });
  }

  async function submit() {
    if (submitting || !view || Boolean(view.launch_note)) return;
    submitting = true;
    error = '';
    try {
      await queue;
      if (!view || Boolean(view.launch_note)) return;
      // + Agents joins the swarm on its own prompt and keep-going choice (F16).
      onResult(
        request.into === null
          ? await goalSwarmCommands.launch({
              project_identifier: view.project_identifier,
              prompt,
              sources: view.choices.sources,
              mix: view.choices.rows,
              keep_going: keepGoing,
            })
          : await goalSwarmCommands.addAgents({
              project_identifier: view.project_identifier,
              swarm_identifier: Number(request.into),
              mix: view.choices.rows,
            }),
      );
    } catch (reason) {
      report(reason);
    } finally {
      submitting = false;
    }
  }
</script>

<dialog
  bind:this={dialogElement}
  transition:modal|global
  class="duty-dialog goal-dialog"
  aria-labelledby="goal-dialog-title"
  oncancel={(event) => {
    event.preventDefault();
    dismiss();
  }}
  onclick={(event) => {
    if (event.target === dialogElement) dismiss();
  }}
>
  <div class="np gs">
    <div class="np-h">
      <h2 id="goal-dialog-title"><Icon name="goal" />Goal swarm</h2>
      <span class="label"
        >one prompt · many agents · they split the work in AIRC · Esc to cancel</span
      >
    </div>
    <div class="np-body">
      <label class="field">
        <span class="label">Prompt · every agent gets exactly this</span>
        <textarea
          class="brief"
          rows="2"
          spellcheck="false"
          disabled={request.into !== null}
          bind:value={prompt}></textarea>
      </label>

      <div class="field">
        <span class="label">Velocity adds to every agent</span>
        <div class="checks">
          <label class="check locked">
            <input type="checkbox" checked disabled />
            <span>{view?.workspace_rule ?? ''}</span>
            <span class="lockt"><Icon name="lock" /> always</span>
          </label>
          <label class="check locked">
            <input type="checkbox" checked disabled />
            {view?.completion_rule ?? ''}
            <span class="lockt"><Icon name="lock" /> AGENTS.md</span>
          </label>
          <label class="check locked">
            <input type="checkbox" checked disabled />
            Join {view?.channel ?? ''} in AIRC and claim work before editing · first
            claim wins
            <span class="lockt"><Icon name="lock" /> always</span>
          </label>
        </div>
      </div>

      <div class="field">
        <span class="label">Agents · all start at once</span>
        <div class="gs-n">
          <span class="gs-step lg" role="group" aria-label="Agents">
            <button
              type="button"
              title="Fewer"
              onclick={() => draw({ kind: 'step_agents', up: false })}>−</button
            >
            <b>{view?.choices.agents ?? ''}</b>
            <button
              type="button"
              title="More"
              onclick={() => draw({ kind: 'step_agents', up: true })}>+</button
            >
          </span>
          <span class="gs-quick">
            {#each view?.quick_picks ?? [] as count (count)}
              <button
                type="button"
                class:on={view?.choices.agents === count}
                aria-pressed={view?.choices.agents === count}
                onclick={() => draw({ kind: 'set_agents', count })}
                >{count}</button
              >
            {/each}
          </span>
          <span class="hint">{view?.worktrees_hint ?? ''}</span>
        </div>
      </div>

      <div class="field">
        <span class="label">Mix · how many run on each model</span>
        <div class="gs-mix">
          <div class="gs-bar">
            {#each view?.segments ?? [] as segment, index (index)}
              <span
                class={segment.provider}
                style:flex={segment.weight}
                title={segment.title}>{segment.label}</span
              >
            {/each}
          </div>
          {#each view?.rows ?? [] as row, index (index)}
            <div class="gs-row">
              <span class="pi"><Icon name={providerIcon(row.provider)} /></span>
              <select
                class="gs-sel"
                aria-label="Model"
                value={row.provider}
                onchange={(event) => {
                  const provider = view?.models.find(
                    (model) => model.provider === event.currentTarget.value,
                  )?.provider;
                  if (provider) draw({ kind: 'set_model', index, provider });
                }}
              >
                {#each view?.models ?? [] as model (model.provider)}
                  <optgroup label={model.provider_name}>
                    <option value={model.provider}>{model.model_name}</option>
                  </optgroup>
                {/each}
              </select>
              <div class="seg sm" role="group" aria-label="Effort">
                {#each efforts as [effort, label] (effort)}
                  <button
                    type="button"
                    class:on={row.effort === effort}
                    aria-pressed={row.effort === effort}
                    onclick={() => draw({ kind: 'set_effort', index, effort })}
                    >{label}</button
                  >
                {/each}
              </div>
              <div
                class="seg sm"
                class:dis={!row.fast_available}
                title={row.fast_title}
              >
                <button
                  type="button"
                  class:on={row.fast}
                  aria-pressed={row.fast}
                  disabled={!row.fast_available}
                  onclick={() => draw({ kind: 'toggle_fast', index })}
                  ><Icon name="bolt" />Fast</button
                >
              </div>
              <span
                class="gs-step"
                role="group"
                aria-label="Agents on this model"
              >
                <button
                  type="button"
                  title="One fewer"
                  onclick={() => draw({ kind: 'step_row', index, up: false })}
                  >−</button
                >
                <b>{row.count}</b>
                <button
                  type="button"
                  title="One more"
                  onclick={() => draw({ kind: 'step_row', index, up: true })}
                  >+</button
                >
              </span>
              <span class="tag">{row.rest ? 'the rest' : ''}</span>
              {#if row.rest}<span></span>{:else}<button
                  type="button"
                  class="x"
                  title="Remove this model"
                  onclick={() => draw({ kind: 'remove_row', index })}>×</button
                >{/if}
            </div>
          {/each}
          <div class="gs-tools">
            <button
              type="button"
              class="mini"
              onclick={() => draw({ kind: 'add_row' })}
              ><Icon name="plus" /> Add a model</button
            >
            {#if (view?.rows.length ?? 0) > 1}<button
                type="button"
                class="mini"
                onclick={() => draw({ kind: 'even_split' })}>Even split</button
              >{/if}
            <span class="hint">{view?.nick_preview ?? ''}</span>
          </div>
        </div>
        <div class="gs-lim">
          {#each view?.limits ?? [] as limit (limit.provider)}
            <span class:warn={limit.warn}
              ><Icon name={providerIcon(limit.provider)} />{limit.text}</span
            >
          {/each}
        </div>
        <div
          class="models"
          role="group"
          aria-label="Sign in to another provider"
        >
          <ProviderSignInCards {send} {report} />
        </div>
      </div>

      <div class="field">
        <span class="label">Choose work from</span>
        <div class="checks">
          {#each workSources as [source, label] (source)}
            <label class="check">
              <input
                type="checkbox"
                checked={view?.choices.sources[source] ?? true}
                disabled={!view || request.into !== null}
                onchange={(event) =>
                  setSource(source, event.currentTarget.checked)}
              />
              {label}
            </label>
          {/each}
        </div>
        {#if view?.pool}<div class="gs-pool" class:warn={view.pool.spare}>
            They choose from <b>{view.pool.pieces}</b>: {view.pool.sources}
            {#if view.pool.spare}<b class="sp">{view.pool.spare}</b>{/if}
          </div>{:else}<div class="gs-pool">{view?.pool_note ?? ''}</div>{/if}
        <div class="checks">
          {#if request.into === null}<label class="check">
              <input type="checkbox" bind:checked={keepGoing} />
              {view?.keep_going_label ?? ''}
            </label>{/if}
        </div>
      </div>
    </div>
    <div class="np-f">
      {#if error}<p class="np-error" role="alert">{error}</p>{:else}<span
          class="hint"
          >{[view?.summary, view?.launch_note]
            .filter(Boolean)
            .join(' · ')}</span
        >{/if}
      <span class="spacer"></span>
      <button type="button" class="btn" onclick={dismiss}>Cancel</button>
      <button
        type="button"
        class="btn solid"
        disabled={submitting || !view || Boolean(view.launch_note)}
        title={view?.launch_note ?? undefined}
        onclick={() => void submit()}
      >
        {view?.launch_label ?? 'Launch'}
        <kbd>Ctrl</kbd><kbd>↵</kbd>
      </button>
    </div>
  </div>
</dialog>
