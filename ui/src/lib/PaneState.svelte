<script lang="ts">
  import { onMount } from 'svelte';
  import { paneConnection, paneFailure } from './pane-state.svelte';
  let {
    name,
    loading = false,
    empty = false,
    message = 'Nothing here yet.',
    error = '',
    retry,
    rosen = false,
    network = true,
    automatic = true,
  }: {
    name: string;
    loading?: boolean;
    empty?: boolean;
    message?: string;
    error?: string;
    retry?: () => void | Promise<void>;
    rosen?: boolean;
    network?: boolean;
    automatic?: boolean;
  } = $props();
  let retrying = $state(false);
  let retryError = $state('');
  const failure = $derived(retryError || error);
  const disconnected = $derived(
    (network && !paneConnection.online) ||
      (rosen && paneConnection.rosenReconnecting),
  );
  async function refresh() {
    if (!retry || retrying) return;
    retrying = true;
    retryError = '';
    try {
      await retry();
    } catch (reason) {
      retryError = String(reason);
    } finally {
      retrying = false;
    }
  }
  // Connection recovery repeats reads; retrying an agent turn takes an explicit click.
  onMount(() => {
    if (!automatic || !retry) return;
    const timer = globalThis.setInterval(() => {
      if (automatic && failure && !disconnected) void refresh();
    }, 5000);
    const online = () => {
      if (automatic && network) void refresh();
    };
    window.addEventListener('online', online);
    return () => {
      globalThis.clearInterval(timer);
      window.removeEventListener('online', online);
    };
  });
  let wasReconnecting = false;
  $effect(() => {
    const reconnecting = rosen && paneConnection.rosenReconnecting;
    if (automatic && wasReconnecting && !reconnecting) void refresh();
    wasReconnecting = reconnecting;
  });
</script>

{#if disconnected || failure || loading || empty}
  <div
    class="pane-state"
    role={failure && !disconnected ? 'alert' : 'status'}
    aria-live="polite"
    aria-busy={loading || disconnected}
  >
    {#if disconnected}
      <span
        >{rosen && paneConnection.rosenReconnecting
          ? 'Rosen disconnected · Reconnecting automatically…'
          : `${name} offline · Waiting for the network…`}</span
      >
    {:else if failure}
      <span>{paneFailure(name, failure, automatic)}</span>
      {#if retry}<button
          class="mini"
          disabled={retrying}
          onclick={() => void refresh()}>Retry</button
        >{/if}
    {:else if loading}
      <span>Loading {name.toLowerCase()}…</span>
      <div class="skeleton" aria-hidden="true">
        <span></span><span></span><span></span>
      </div>
    {:else}<span>{message}</span>{/if}
  </div>
{/if}

<style>
  .pane-state {
    padding: var(--workspaces-nav-padding);
    color: var(--muted);
    font-size: var(--agents-help-size);
    display: grid;
    gap: var(--issue-meta-gap);
  }
  .skeleton {
    display: grid;
    gap: var(--issue-meta-gap);
  }
  .skeleton span {
    display: block;
    height: var(--agents-text-height);
    border-radius: var(--r-sm);
    background: var(--surface-2);
  }
</style>
