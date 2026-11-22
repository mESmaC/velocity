<script lang="ts">
  import { onMount } from 'svelte';
  import './theme/providers.css';
  import './theme/shared-mcp.css';
  import {
    collapseConnectionCommands,
    editSharedMcpServers,
    readSharedMcpServers,
    type CollapseConnection,
    type SharedMcpServer,
    type SharedMcpServerEdit,
  } from './ipc';
  let servers = $state<SharedMcpServer[]>([]);
  let collapse = $state<CollapseConnection | null>(null);
  let collapseBusy = $state(false);
  let collapseError = $state('');
  let name = $state('');
  let target = $state('');
  let error = $state('');
  onMount(() => {
    void changeCollapse(collapseConnectionCommands.read);
    void readSharedMcpServers()
      .then((value) => {
        servers = value;
      })
      .catch((failure) => {
        error = String(failure);
      });
  });
  async function edit(change: SharedMcpServerEdit): Promise<boolean> {
    try {
      servers = await editSharedMcpServers(change);
      error = '';
      return true;
    } catch (failure) {
      error = String(failure);
      return false;
    }
  }
  // Collapse's read-only MCP server (#430): Connect waits for the browser approval.
  async function changeCollapse(change: () => Promise<CollapseConnection>) {
    collapseBusy = true;
    try {
      collapse = await change();
      collapseError = '';
    } catch (failure) {
      collapseError = String(failure);
    } finally {
      collapseBusy = false;
    }
  }
  async function add() {
    if (await edit({ Add: { name, target } })) {
      name = '';
      target = '';
    }
  }
  function targetText(server: SharedMcpServer): string {
    const transport = server.transport;
    return transport.kind === 'url'
      ? transport.url
      : [transport.program, ...transport.arguments].join(' ');
  }
</script>

<section aria-label="Shared MCP servers">
  <h2>Shared MCP servers</h2>
  <p class="workspace-description">
    Passed to every agent of every provider when its session starts: threads,
    swarms, duty agents and spec interviews. When a name is also in an agent's
    own config, the agent's own entry wins. Servers inherit your environment, so
    there is no field for a variable or a key.
  </p>
  {#if collapse}
    <div class="shared-mcp-row collapse">
      {#if collapse.account}
        <label class="workspace-check"
          ><input
            type="checkbox"
            role="switch"
            checked={collapse.enabled}
            disabled={collapseBusy}
            onchange={(event) => {
              const enabled = event.currentTarget.checked;
              void changeCollapse(() =>
                collapseConnectionCommands.setEnabled(enabled),
              );
            }}
          /> Collapse · landed history, ledgers and code graph</label
        ><span class="target" title={collapse.url}
          >Connected · {collapse.account}</span
        ><span class="kind">http</span><button
          class="workspace-button mini"
          disabled={collapseBusy}
          onclick={() =>
            void changeCollapse(collapseConnectionCommands.disconnect)}
          >Disconnect</button
        >
      {:else}
        <span class="name">{collapse.name}</span><span
          class="target"
          title={collapse.url}>Collapse · not connected</span
        ><button
          class="workspace-button mini"
          disabled={collapseBusy}
          onclick={() =>
            void changeCollapse(collapseConnectionCommands.connect)}
          >{collapseBusy
            ? 'Waiting for the browser…'
            : 'Sign in with Collapse'}</button
        >
      {/if}
    </div>
    <p class="workspace-description">
      Collapse serves read-only data across connected projects. Sign in through
      the browser; credentials stay in the OS credential store.
    </p>
  {/if}
  {#if collapseError}<p class="workspace-message error" role="alert">
      {collapseError}
    </p>{/if}
  {#each servers as server (server.name)}
    <div class="shared-mcp-row">
      <span class="name">{server.name}</span><span
        class="target"
        title={targetText(server)}>{targetText(server)}</span
      ><span class="kind"
        >{server.transport.kind === 'url' ? 'http' : 'stdio'}</span
      ><button
        class="workspace-button mini"
        aria-label={`Remove ${server.name}`}
        onclick={() => void edit({ Remove: { name: server.name } })}
        >Remove</button
      >
    </div>
  {/each}
  <form
    class="pv-add"
    onsubmit={(event) => {
      event.preventDefault();
      void add();
    }}
  >
    <div class="pv-add-row">
      <label class="workspace-input"
        ><span>Name</span><input
          aria-label="Server name"
          placeholder="github"
          bind:value={name}
          spellcheck="false"
        /></label
      >
      <label class="workspace-input pv-add-command"
        ><span>Command or URL</span><input
          aria-label="Server command and arguments, or URL"
          placeholder="npx -y @modelcontextprotocol/server-github"
          bind:value={target}
          spellcheck="false"
        /></label
      >
      <button class="workspace-button" type="submit">Add server</button>
    </div>
  </form>
  {#if error}<p class="workspace-message error" role="alert">{error}</p>{/if}
  <p class="workspace-description">
    New agents get the list; running agents keep theirs.
  </p>
</section>
