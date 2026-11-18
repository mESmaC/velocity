<script lang="ts">
  import type { GraphStatus } from './ipc';

  let {
    status,
    retry,
    busy,
    enabled,
  }: {
    status: GraphStatus;
    retry: () => void;
    busy: boolean;
    enabled: boolean;
  } = $props();
</script>

<div class="usage-graph-status">
  <div class="usage-graph-foot">
    <strong class="usage-state" data-state={status.state}>{status.state}</strong
    >
    {#if status.state === 'Failed' && enabled}
      <button class="usage-button" disabled={busy} onclick={retry}>Retry</button
      >
    {/if}
  </div>
  <p class="usage-note" role="status">{status.detail}</p>
  {#if status.state === 'Indexing'}
    <progress
      class="usage-index-progress"
      aria-label="Base index progress"
      max="100"
      value={status.progress ?? undefined}
    ></progress>
  {/if}
  {#if status.base}
    <p class="usage-note">
      Base indexed <code title={status.base.commit}
        >{status.base.commit.slice(0, 12)}</code
      >
      · Updated {status.base.updated}
    </p>
  {:else}
    <p class="usage-note">No base commit indexed yet.</p>
  {/if}
  <p class="usage-note">
    {status.catching_up.length
      ? 'Workspaces catching up · ' + status.catching_up.join(', ')
      : 'Workspaces catching up · None'}
  </p>
  <p class="usage-note">
    Parse cache · {status.cache_size} · {status.base &&
    status.base.hit_rate !== '—'
      ? status.base.hit_rate + ' hit rate on last base update'
      : 'Hit rate unavailable'}
  </p>
</div>
{#if status.base}
  <details class="usage-graph-details">
    <summary>Coverage</summary>
    <table class="usage-compare" aria-label="Code graph coverage">
      <thead
        ><tr
          ><th scope="col">Language</th><th scope="col">Files</th><th
            scope="col">Symbols</th
          ><th scope="col">Edges</th></tr
        ></thead
      >
      <tbody>
        {#each status.base.languages as row (row.language)}
          <tr
            ><th scope="row">{row.language}</th><td>{row.files}</td><td
              >{row.symbols}</td
            ><td>{row.edges}</td></tr
          >
        {:else}
          <tr><td colspan="4">No files indexed.</td></tr>
        {/each}
      </tbody>
    </table>
    <p class="usage-note">
      Skipped parsing · {status.base.skipped.length
        ? status.base.skipped
            .map((row) => row.files + ' ' + row.reason.toLowerCase())
            .join(' · ')
        : 'None'}
    </p>
  </details>
{/if}
<details class="usage-graph-details">
  <summary>Tool calls</summary>
  <table class="usage-compare" aria-label="Graph tool calls">
    <thead
      ><tr
        ><th scope="col">Tool</th><th scope="col">Calls</th><th scope="col"
          >Exact</th
        ><th scope="col">Classified</th></tr
      ></thead
    >
    <tbody>
      {#each status.tools as tool (tool.name)}
        <tr
          ><th scope="row">{tool.name}</th><td>{tool.calls}</td><td
            >{tool.exact}</td
          ><td>{tool.classified}</td></tr
        >
      {/each}
    </tbody>
  </table>
  <p class="usage-note">
    {status.access}. Other outcomes include ranked matches and request errors.
  </p>
  {#if status.threads.length}
    <table
      class="usage-compare"
      aria-label="Reported tokens and code access by thread"
    >
      <thead
        ><tr
          ><th scope="col">Thread</th><th scope="col">Input</th><th scope="col"
            >Cached input</th
          ><th scope="col">Output</th><th scope="col">Graph calls</th><th
            scope="col">Raw reads and searches</th
          ><th scope="col">Whole-file reads</th></tr
        ></thead
      >
      <tbody>
        {#each status.threads as thread (thread.key)}
          <tr
            ><th scope="row">{thread.name}</th><td>{thread.input}</td><td
              >{thread.cached_input}</td
            ><td>{thread.output}</td><td>{thread.graph_calls}</td><td
              >{thread.raw_reads}</td
            ><td>{thread.whole_file_reads}</td></tr
          >
        {/each}
      </tbody>
    </table>
  {/if}
</details>
