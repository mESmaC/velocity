<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { goalSwarmCommands } from './ipc';
  import type {
    EffortLevel,
    GoalDialogRequest,
    GoalMixEdit,
    GoalSwarmChoices,
    GoalSwarmDialog,
    LayoutAction,
    LayoutSnapshot,
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

  let dialogElement: HTMLDialogElement;
  let view = $state<GoalSwarmDialog | null>(null);
  // `null` asks Rust for the defaults; afterwards it echoes the choices it drew.
  let choices: GoalSwarmChoices | null = null;
  let prompt = $state('');
  let keepGoing = $state(false);
  let addPrDuty = $state(true);
  let submitting = $state(false);
  let error = $state('');
  let handledSubmits = 0;
  // Edits are applied in Rust one after another, each on the choices the last one returned.
  let queue: Promise<void> = Promise.resolve();

  onMount(() => {
    const previous = document.activeElement;
    prompt = request.prompt;
    handledSubmits = submitRequests;
    dialogElement.showModal();
    draw();
    // The pool is read from GitHub and SPEC.md; the dialog is usable meanwhile.
    goalSwarmCommands
      .readPool()
      .then(() => draw())
      .catch(report);
    return () => {
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus();
    };
  });

  function report(reason: unknown) {
    error = reason instanceof Error ? reason.message : String(reason);
  }

  function draw(edit: GoalMixEdit | null = null) {
    queue = queue
      .then(() => goalSwarmCommands.dialog({ choices, edit }))
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
      onResult(
        await goalSwarmCommands.launch({
          project_identifier: view.project_identifier,
          prompt,
          mix: view.choices.rows,
          keep_going: keepGoing,
          add_pr_duty: addPrDuty && view.pr_duty_shortfall > 0,
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
        <textarea class="brief" rows="2" spellcheck="false" bind:value={prompt}
        ></textarea>
      </label>

      <div class="field">
        <span class="label">Velocity adds to every agent</span>
        <div class="checks">
          <label class="check locked">
            <input type="checkbox" checked disabled />
            <span
              >Its own worktree under <code>{view?.worktree_root ?? ''}</code>,
              removed after its PR merges</span
            >
            <span class="lockt"><Icon name="lock" /> always</span>
          </label>
          <label class="check locked">
            <input type="checkbox" checked disabled />
            Commit, push its branch, open a PR against {view?.default_branch ??
              ''}, watch it until it merges
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
              <span class="pi"><Icon name={row.provider} /></span>
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
              ><Icon name={limit.provider} />{limit.text}</span
            >
          {/each}
        </div>
      </div>

      <div class="field">
        {#if view?.pool}<div class="gs-pool" class:warn={view.pool.spare}>
            They choose from <b>{view.pool.pieces}</b>: {view.pool.sources}
            {#if view.pool.spare}<b class="sp">{view.pool.spare}</b>{/if}
          </div>{:else}<div class="gs-pool">{view?.pool_note ?? ''}</div>{/if}
        <div class="checks">
          <label class="check">
            <input type="checkbox" bind:checked={keepGoing} />
            Keep going: after its PR merges, an agent claims the next unclaimed piece
            instead of stopping
          </label>
          {#if view?.pr_duty_label && view.pr_duty_shortfall > 0}<label
              class="check"
            >
              <input type="checkbox" bind:checked={addPrDuty} />
              {view.pr_duty_label}
            </label>{:else if view?.pr_duty_label}<span class="hint"
              >{view.pr_duty_label}</span
            >{/if}
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
