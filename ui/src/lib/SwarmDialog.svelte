<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { swarmCommands } from './ipc';
  import type {
    LayoutAction,
    LayoutSnapshot,
    ProviderKind,
    SwarmDialog,
    SwarmDialogRequest,
    SwarmItem,
    SwarmSource,
  } from './ipc';
  import './theme/new-project.css';
  import './theme/swarm.css';

  let {
    request,
    send,
    submitRequests,
    onResult,
  }: {
    request: SwarmDialogRequest;
    send: (action: LayoutAction) => void;
    submitRequests: number;
    onResult: (snapshot: LayoutSnapshot) => void;
  } = $props();

  const providerNames: Record<ProviderKind, string> = {
    claude: 'Anthropic',
    codex: 'Codex',
    antigravity: 'Antigravity',
    kimi: 'Kimi',
  };

  let dialogElement: HTMLDialogElement;
  let view = $state<SwarmDialog | null>(null);
  let source = $state<SwarmSource>('tasks');
  // `null` is the default: every open item checked (F10).
  let chosen = $state<SwarmItem[] | null>(null);
  let providers = $state<ProviderKind[]>([]);
  let closeIssues = $state(true);
  let rebaseOnMerge = $state(true);
  let addPrDuty = $state(true);
  let submitting = $state(false);
  let error = $state('');
  let handledSubmits = 0;
  let requestNumber = 0;

  const into = $derived(request.into === null ? null : Number(request.into));
  const selectable = $derived(
    view?.items.filter((item) => item.selectable) ?? [],
  );
  const picked = $derived(view?.items.filter((item) => item.selected) ?? []);

  onMount(() => {
    const previous = document.activeElement;
    source = request.source;
    handledSubmits = submitRequests;
    dialogElement.showModal();
    refresh();
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

  // Rust draws the dialog from the choices; a late answer never replaces a newer one.
  function refresh() {
    const number = ++requestNumber;
    swarmCommands
      .dialog({ source, items: chosen, providers, into })
      .then((next) => {
        if (number !== requestNumber) return;
        view = next;
        providers = next.models
          .filter((model) => model.selected)
          .map((model) => model.provider);
      })
      .catch(report);
  }

  function same(left: SwarmItem, right: SwarmItem): boolean {
    return left.kind === right.kind && left.number === right.number;
  }

  function chooseSource(next: SwarmSource) {
    source = next;
    chosen = null;
    refresh();
  }

  function toggleItem(item: SwarmItem, checked: boolean) {
    const current = picked.map((entry) => entry.item);
    chosen = checked
      ? [...current, item]
      : current.filter((entry) => !same(entry, item));
    refresh();
  }

  function toggleAll() {
    chosen = picked.length === selectable.length ? [] : null;
    refresh();
  }

  function toggleModel(provider: ProviderKind) {
    if (providers.includes(provider)) {
      if (providers.length > 1)
        providers = providers.filter((entry) => entry !== provider);
    } else {
      providers = [...providers, provider];
    }
    refresh();
  }

  function dismiss() {
    if (!submitting) send({ Request: { Navigation: 'Dismiss' } });
  }

  async function submit() {
    if (submitting || !view || picked.length === 0) return;
    submitting = true;
    error = '';
    try {
      const launch = {
        project_identifier: view.project_identifier,
        source,
        items: picked.map((entry) => entry.item),
        providers,
        close_issues: closeIssues,
        rebase_on_merge: rebaseOnMerge,
        add_pr_duty: addPrDuty && view.pr_duty_shortfall > 0,
        into,
      };
      onResult(
        into === null
          ? await swarmCommands.launch(launch)
          : await swarmCommands.addItems(launch),
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
  class="duty-dialog swarm-dialog"
  aria-labelledby="swarm-dialog-title"
  oncancel={(event) => {
    event.preventDefault();
    dismiss();
  }}
  onclick={(event) => {
    if (event.target === dialogElement) dismiss();
  }}
>
  <div class="np sw">
    <div class="np-h">
      <h2 id="swarm-dialog-title"><Icon name="swarm" />Swarm</h2>
      <span class="label">one agent per item · all at once · Esc to cancel</span
      >
    </div>
    <div class="np-body">
      <div class="field">
        <span class="label">Source</span>
        <div class="seg" role="group" aria-label="Source">
          {#each [['tasks', 'Task List', view?.task_count], ['issues', 'Issues', view?.issue_count], ['both', 'Both', (view?.task_count ?? 0) + (view?.issue_count ?? 0)]] as const as [value, label, count] (value)}
            <button
              type="button"
              class:on={source === value}
              aria-pressed={source === value}
              disabled={into !== null}
              onclick={() => chooseSource(value)}>{label} · {count ?? 0}</button
            >
          {/each}
        </div>
      </div>

      <div class="field">
        <div class="row2 spread">
          <span class="label">Items · one agent and one worktree each</span>
          <button
            type="button"
            class="mini"
            disabled={selectable.length === 0}
            onclick={toggleAll}
            >{picked.length === selectable.length && selectable.length
              ? 'Select none'
              : 'Select all'}</button
          >
        </div>
        <div class="sw-items" role="group" aria-label="Items">
          {#each view?.items ?? [] as entry (`${entry.item.kind}-${entry.item.number}`)}
            <label class="sw-it" class:dis={!entry.selectable}>
              <input
                type="checkbox"
                checked={entry.selected}
                disabled={!entry.selectable}
                onchange={(event) =>
                  toggleItem(entry.item, event.currentTarget.checked)}
              />
              <Icon name={entry.issue ? 'issue' : 'circle'} />
              <span class="ttl">{entry.title}</span>
              <span class="ref">{entry.note}</span>
            </label>
          {:else}
            <div class="sw-empty">
              {view?.issues_note ?? 'Nothing open here.'}
            </div>
          {/each}
        </div>
      </div>

      <div class="field">
        <span class="label" id="swarm-model-label"
          >Models · pick several to spread the swarm</span
        >
        <div class="models" role="group" aria-labelledby="swarm-model-label">
          {#each view?.models ?? [] as model (model.provider)}
            <button
              type="button"
              class="model-card"
              class:on={model.selected}
              aria-pressed={model.selected}
              onclick={() => toggleModel(model.provider)}
            >
              <span class="mh"
                ><Icon name={model.provider} />{model.model_name}</span
              >
              <span class="md">{providerNames[model.provider]}</span>
            </button>
          {/each}
        </div>
        <div class="hint">{view?.split_label ?? ''}</div>
      </div>

      <div class="field">
        <div class="checks">
          <label class="check locked">
            <input type="checkbox" checked disabled />
            Each agent opens a PR and watches it until it merges
            <span class="lockt"><Icon name="lock" /> AGENTS.md</span>
          </label>
          <label class="check locked">
            <input type="checkbox" checked disabled />
            <span
              >Each agent works in its own worktree under <code
                >{view?.worktree_root ?? ''}</code
              >, deleted when its PR merges</span
            >
            <span class="lockt"><Icon name="lock" /> always</span>
          </label>
          <label class="check">
            <input type="checkbox" bind:checked={rebaseOnMerge} />
            Rebase a member when another swarm PR that touches its files merges
          </label>
          {#if source !== 'tasks'}<label class="check">
              <input type="checkbox" bind:checked={closeIssues} />
              Close each issue when its PR merges
            </label>{/if}
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
          class="hint">{view?.summary ?? ''}</span
        >{/if}
      <span class="spacer"></span>
      <button type="button" class="btn" disabled={submitting} onclick={dismiss}>
        Cancel
      </button>
      <button
        type="button"
        class="btn solid"
        disabled={submitting || picked.length === 0}
        onclick={submit}
      >
        {view?.launch_label ?? 'Launch'}
        <kbd>Ctrl</kbd><kbd>↵</kbd>
      </button>
    </div>
  </div>
</dialog>
