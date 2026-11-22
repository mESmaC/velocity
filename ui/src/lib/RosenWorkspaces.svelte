<script lang="ts">
  import { onMount } from 'svelte';
  import MachinesPanel from './MachinesPanel.svelte';
  import PaneState from './PaneState.svelte';
  import {
    workspaceCommands,
    type LandedWorkspacePreparation,
    type LayoutAction,
    type WorkspacePreparation,
    type WorkspaceRow,
  } from './ipc';

  let {
    project,
    send,
  }: {
    project: number;
    send: (action: LayoutAction) => void;
  } = $props();

  let rows = $state<WorkspaceRow[] | null>(null);
  let configuration = $state<LandedWorkspacePreparation | null | undefined>();
  let rowError = $state('');
  let configurationError = $state('');
  let actionError = $state('');
  let deleting = $state<string | null>(null);
  let saving = $state(false);
  let change = $state('');
  let draft = $state<{
    original: WorkspacePreparation;
    size: number | undefined;
    commands: string;
  } | null>(null);
  let refreshing = false;
  let mounted = false;
  let rowRequest = 0;

  function startEditing() {
    if (!draft && configuration) {
      const original = configuration.preparation;
      draft = {
        original,
        size: original.warm_pool_size,
        commands: original.prepare_commands.join('\n'),
      };
    }
  }

  async function loadRows() {
    const request = ++rowRequest;
    try {
      const result = await workspaceCommands.list(project);
      if (mounted && request === rowRequest) {
        rows = result;
        rowError = '';
      }
    } catch (reason) {
      if (mounted && request === rowRequest) rowError = String(reason);
    }
  }

  async function loadPreparation() {
    try {
      const result = await workspaceCommands.preparation(project);
      if (mounted) {
        configuration = result;
        configurationError = '';
      }
    } catch (reason) {
      if (mounted) configurationError = String(reason);
    }
  }

  async function refresh() {
    if (refreshing) return;
    refreshing = true;
    try {
      await Promise.all([loadRows(), loadPreparation()]);
    } finally {
      refreshing = false;
    }
  }

  async function propose() {
    if (saving || !draft || draft.size === undefined) return;
    saving = true;
    actionError = '';
    try {
      const edited = {
        warm_pool_size: draft.size,
        prepare_commands: draft.commands
          .split(/\r?\n/)
          .filter((line) => line.trim()),
      };
      change =
        (await workspaceCommands.propose(project, draft.original, edited)) ??
        '';
      draft = null;
      await loadPreparation();
    } catch (reason) {
      actionError = String(reason);
    } finally {
      saving = false;
    }
  }

  async function remove(row: WorkspaceRow) {
    if (
      deleting !== null ||
      !window.confirm(`Delete the workspace for change ${row.change}?`)
    )
      return;
    deleting = row.path;
    actionError = '';
    try {
      await workspaceCommands.delete(project, row.change, row.path);
      await loadRows();
    } catch (reason) {
      actionError = String(reason);
    } finally {
      deleting = null;
    }
  }

  onMount(() => {
    mounted = true;
    void refresh();
    const timer = window.setInterval(() => void refresh(), 4000);
    return () => {
      mounted = false;
      window.clearInterval(timer);
    };
  });
</script>

{#if configuration !== null}
  <section>
    <h2>Warm pool</h2>
    <p class="workspace-description">
      Rosen reads these settings from <code>rosen.toml</code>. Edits apply after
      the change lands.
    </p>
    <PaneState
      name="Warm pool settings"
      network={false}
      loading={!configuration}
      error={configurationError}
      retry={refresh}
      automatic={false}
    />
    {#if configuration}
      {#if configuration.config_error}
        <p class="workspace-message error" role="alert">
          {configuration.config_error}
        </p>
      {/if}
      <form
        onsubmit={(event) => {
          event.preventDefault();
          void propose();
        }}
      >
        <div class="workspace-setting-row">
          <label for="workspace-pool-size">Workspaces to keep ready</label>
          <label class="workspace-input">
            <input
              id="workspace-pool-size"
              type="number"
              required
              min="0"
              max={configuration.maximum_pool_size}
              step="1"
              value={draft
                ? draft.size
                : configuration.preparation.warm_pool_size}
              disabled={saving || Boolean(configuration.config_error)}
              oninput={(event) => {
                startEditing();
                if (draft) {
                  const size = event.currentTarget.valueAsNumber;
                  draft.size = Number.isNaN(size) ? undefined : size;
                }
              }}
            />
          </label>
        </div>
        <div class="workspace-commands">
          <label for="workspace-prepare">Prepare commands</label>
          <p id="workspace-prepare-help" class="workspace-description">
            One command per line.
          </p>
          <textarea
            id="workspace-prepare"
            aria-describedby="workspace-prepare-help"
            rows="4"
            spellcheck="false"
            disabled={saving || Boolean(configuration.config_error)}
            value={draft?.commands ??
              configuration.preparation.prepare_commands.join('\n')}
            oninput={(event) => {
              startEditing();
              if (draft) draft.commands = event.currentTarget.value;
            }}></textarea>
        </div>
        <div class="workspace-summary">
          <button
            type="submit"
            class="workspace-button"
            disabled={!draft || saving || Boolean(configuration.config_error)}
          >
            {saving ? 'Creating change…' : 'Create change'}
          </button>
          {#if draft}
            <button
              type="button"
              class="workspace-button"
              disabled={saving}
              onclick={() => (draft = null)}>Reset</button
            >
          {/if}
        </div>
      </form>
      {#if change}
        <p class="workspace-message" role="status">
          Created Rosen change <code>{change}</code>.
        </p>
      {/if}
    {/if}
  </section>
{/if}

{#if actionError}
  <p class="workspace-message error" role="alert">{actionError}</p>
{/if}

<section>
  <h2>Live workspaces</h2>
  <PaneState
    name="Workspaces"
    network={false}
    loading={!rows}
    empty={rows?.length === 0}
    message="No Rosen workspaces on this machine."
    error={rowError}
    retry={refresh}
    automatic={false}
  />
  {#if rows?.length}
    <p class="workspace-description">
      {rows.length} on this machine. Sizes exclude linked directories.
    </p>
    <table class="workspace-inventory">
      <thead>
        <tr
          ><th scope="col">Change</th><th scope="col">Thread</th><th scope="col"
            >Size</th
          ><th scope="col">State</th><th scope="col" aria-label="Actions"
          ></th></tr
        >
      </thead>
      <tbody>
        {#each rows as row (row.path)}
          <tr>
            <td
              ><code title={`${row.change}\n${row.path}`}>{row.change}</code
              ></td
            >
            <td>
              {#if row.thread}
                {@const thread = row.thread}
                <button
                  title={thread.title}
                  onclick={() => send({ OpenThread: thread.identifier })}
                  >{thread.title}</button
                >
              {:else}
                —
              {/if}
            </td>
            <td>{row.size ?? '—'}</td>
            <td title={row.kept ?? undefined}
              >{row.kept ? `${row.state} · kept` : row.state}</td
            >
            <td>
              <button
                class="workspace-button mini"
                aria-label={`Delete workspace for ${row.change}`}
                disabled={deleting !== null}
                onclick={() => void remove(row)}
              >
                {deleting === row.path ? 'Deleting…' : 'Delete'}
              </button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</section>

{#if configuration}
  <MachinesPanel {project} />
{/if}
