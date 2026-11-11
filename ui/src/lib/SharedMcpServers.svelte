<script lang="ts">
  import { onMount } from 'svelte';
  import './theme/providers.css';
  import './theme/shared-mcp.css';
  import {
    editSharedMcpServers,
    readSharedMcpServers,
    type SharedMcpServer,
    type SharedMcpServerEdit,
  } from './ipc';
  let servers = $state<SharedMcpServer[]>([]);
  let name = $state('');
  let target = $state('');
  let error = $state('');
  onMount(() => {
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
  <p class="worktree-description">
    Passed to every agent of every provider when its session starts: threads,
    swarms, duty agents and spec interviews. When a name is also in an agent's
    own config, the agent's own entry wins. Servers inherit your environment, so
    there is no field for a variable or a key.
  </p>
  {#each servers as server (server.name)}
    <div class="shared-mcp-row">
      <span class="name">{server.name}</span><span
        class="target"
        title={targetText(server)}>{targetText(server)}</span
      ><span class="kind"
        >{server.transport.kind === 'url' ? 'http' : 'stdio'}</span
      ><button
        class="worktree-button mini"
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
      <label class="worktree-input"
        ><span>Name</span><input
          aria-label="Server name"
          placeholder="github"
          bind:value={name}
          spellcheck="false"
        /></label
      >
      <label class="worktree-input pv-add-command"
        ><span>Command or URL</span><input
          aria-label="Server command and arguments, or URL"
          placeholder="npx -y @modelcontextprotocol/server-github"
          bind:value={target}
          spellcheck="false"
        /></label
      >
      <button class="worktree-button" type="submit">Add server</button>
    </div>
  </form>
  {#if error}<p class="worktree-message error" role="alert">{error}</p>{/if}
  <p class="worktree-description">
    New agents get the list; running agents keep theirs.
  </p>
</section>
