<script lang="ts">
  import { onMount } from 'svelte';
  import { pop } from './motion';
  import Icon from './Icon.svelte';
  import ProviderSignInCards from './ProviderSignInCards.svelte';
  import { dutyCommands, providerIcon } from './ipc';
  import type {
    DutyDialogDefaults,
    DutyMergeMethod,
    DutyRole,
    LayoutAction,
    LayoutSnapshot,
    PrDutyRuleSettings,
    ScoutCadence,
    ScoutFocusArea,
    ScoutRuleSettings,
  } from './ipc';
  import './theme/new-project.css';

  let {
    initialRole,
    send,
    submitRequests,
    onResult,
  }: {
    initialRole: DutyRole;
    send: (action: LayoutAction) => void;
    submitRequests: number;
    onResult: (snapshot: LayoutSnapshot) => void;
  } = $props();

  const roles: [DutyRole, string, string, string, string][] = [
    [
      'pr_duty',
      'pr',
      'PR duty',
      'Watches open PRs: reviews the intent and every theorem statement, checks CI and proofs, and merges only what it approves.',
      'With several agents, each one claims a PR so two never review the same one.',
    ],
    [
      'scouts',
      'scout',
      'Scouts',
      'Assumes every part of the app is broken and sets out to prove it: writes the tests and stress scripts, and files each issue with the failing one attached.',
      'With several scouts, each takes a different area of the repo.',
    ],
    [
      'main_watcher',
      'branch',
      'Main watcher',
      'Watches master after every merge: runs every test and stress script, bisects a break to the merge that caused it, and opens a fix or revert PR.',
      'One watcher takes each merge. With several, a merge that lands mid-watch goes to the next one.',
    ],
  ];

  const mergeMethods: [DutyMergeMethod, string][] = [
    ['squash', 'Squash'],
    ['rebase', 'Rebase'],
    ['merge_commit', 'Merge commit'],
  ];

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

  const cadences: [ScoutCadence, string][] = [
    ['once', 'Once'],
    ['every_thirty_minutes', 'Every 30m'],
    ['after_every_merge', 'After every merge'],
  ];

  let dialog: HTMLDialogElement;
  let defaults = $state<DutyDialogDefaults | null>(null);
  let role = $state<DutyRole>('pr_duty');
  let agentCount = $state(1);
  let prRules = $state<PrDutyRuleSettings>({
    review_theorem_statements: true,
    ci_green_before_merge: true,
    check_diff_against_spec: true,
    re_review_on_new_commits: true,
    delete_branch_after_merge: true,
    merge_method: 'squash',
  });
  let scoutRules = $state<ScoutRuleSettings>({
    focus_areas: ['bugs', 'stress', 'proof_gaps', 'spec_drift'],
    assume_every_part_fails: true,
    run_stress_against_models: true,
    label_issues_for_dispatch: false,
    cadence: 'every_thirty_minutes',
  });
  let submitting = $state(false);
  let error = $state('');
  let handledSubmits = 0;

  onMount(() => {
    const previous = document.activeElement;
    role = initialRole;
    handledSubmits = submitRequests;
    dialog.showModal();
    void dutyCommands
      .defaults()
      .then((loaded) => {
        defaults = loaded;
        prRules = loaded.pr_rules;
        scoutRules = loaded.scout_rules;
      })
      .catch(report);
    return () => {
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus();
    };
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

  function roleExists(): boolean {
    if (!defaults) return false;
    if (role === 'pr_duty') return defaults.pr_duty_exists;
    if (role === 'scouts') return defaults.scouts_exists;
    return defaults.main_watcher_exists;
  }

  const selectedRole = $derived(
    roles.find(([value]) => value === role) ?? roles[0],
  );

  async function submit() {
    if (submitting) return;
    submitting = true;
    error = '';
    try {
      const request = {
        role,
        agent_count: agentCount,
        provider: defaults?.provider ?? null,
        model_name: defaults?.model_name ?? null,
        pr_rules: prRules,
        scout_rules: scoutRules,
      };
      const snapshot = roleExists()
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
  in:pop|global
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
      <h2 id="duty-dialog-title">Start a duty agent</h2>
      <span class="label">Esc to cancel</span>
    </div>
    <div class="np-body">
      <fieldset class="field">
        <legend class="label">Role</legend>
        <div class="role-cards" role="group" aria-label="Role">
          {#each roles as [value, icon, name, desc] (value)}
            <button
              type="button"
              class="role-card"
              class:on={role === value}
              aria-pressed={role === value}
              onclick={() => (role = value)}
            >
              <span class="mh"><Icon name={icon} />{name}</span>
              <span class="md">{desc}</span>
            </button>
          {/each}
        </div>
      </fieldset>

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
        <div class="hint">
          {selectedRole[4]}
        </div>
      </div>

      <div class="field">
        <span class="label" id="duty-model-label">Model</span>
        <div class="models" role="group" aria-labelledby="duty-model-label">
          {#if defaults}
            <button type="button" class="model-card on" aria-pressed="true">
              <span class="mh">
                <Icon name={providerIcon(defaults.provider)} />
                {defaults.model_name}
              </span>
              <span class="md">
                {defaults.provider_name}{defaults.model_configured
                  ? ''
                  : ' · default'}
              </span>
            </button>
          {/if}
          <ProviderSignInCards {send} {report} />
        </div>
      </div>

      {#if role === 'pr_duty'}
        <div class="field">
          <span class="label">Rules</span>
          <div class="checks">
            <label class="check locked">
              <input type="checkbox" checked disabled />
              Never merge without an approving review
              <span class="lockt"><Icon name="lock" /> always on</span>
            </label>
            <label class="check locked">
              <input type="checkbox" checked disabled />
              Never merge unless the proofs check: lake build, no sorry, standard
              axioms, statements unchanged since review
              <span class="lockt"><Icon name="lock" /> always on</span>
            </label>
            <label class="check">
              <input
                type="checkbox"
                bind:checked={prRules.review_theorem_statements}
              />
              Review every theorem statement against the claim and SPEC.md
            </label>
            <label class="check">
              <input
                type="checkbox"
                bind:checked={prRules.ci_green_before_merge}
              />
              CI must be green before merging
            </label>
            <label class="check">
              <input
                type="checkbox"
                bind:checked={prRules.check_diff_against_spec}
              />
              Check the diff against SPEC.md and the Task List
            </label>
            <label class="check">
              <input
                type="checkbox"
                bind:checked={prRules.re_review_on_new_commits}
              />
              Re-review when new commits land
            </label>
            <label class="check">
              <input
                type="checkbox"
                bind:checked={prRules.delete_branch_after_merge}
              />
              Delete the branch after merging
            </label>
          </div>
          <div class="row2 spread sub-row">
            <span class="label">Merge method</span>
            <div class="seg sm" role="group" aria-label="Merge method">
              {#each mergeMethods as [value, label] (value)}
                <button
                  type="button"
                  class:on={prRules.merge_method === value}
                  aria-pressed={prRules.merge_method === value}
                  onclick={() => (prRules.merge_method = value)}>{label}</button
                >
              {/each}
            </div>
          </div>
        </div>
      {:else if role === 'main_watcher'}
        <div class="field">
          <span class="label">Rules</span>
          <div class="checks">
            <label class="check locked">
              <input type="checkbox" checked disabled />
              Never push to master: every fix or revert is a PR
              <span class="lockt"><Icon name="lock" /> always on</span>
            </label>
            <label class="check locked">
              <input type="checkbox" checked disabled />
              After each merge: every test and stress script, then a bisect to the
              merge that broke master
              <span class="lockt"><Icon name="lock" /> always on</span>
            </label>
            <label class="check locked">
              <input type="checkbox" checked disabled />
              File each flaky test once, with failing evidence
              <span class="lockt"><Icon name="lock" /> always on</span>
            </label>
          </div>
        </div>
      {:else}
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
          </div>
          <div class="row2 spread sub-row">
            <span class="label">Sweep</span>
            <div class="seg sm" role="group" aria-label="Sweep">
              {#each cadences as [value, label] (value)}
                <button
                  type="button"
                  class:on={scoutRules.cadence === value}
                  aria-pressed={scoutRules.cadence === value}
                  onclick={() => (scoutRules.cadence = value)}>{label}</button
                >
              {/each}
            </div>
          </div>
        </div>
      {/if}
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
        {roleExists()
          ? `Add ${agentCount} to ${selectedRole[2]}`
          : 'Start duty'}
        <kbd>Ctrl</kbd><kbd>↵</kbd>
      </button>
    </div>
  </div>
</dialog>
