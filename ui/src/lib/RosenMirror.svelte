<script lang="ts">
  import type { MirrorState } from './ipc';

  let { state }: { state: MirrorState | null } = $props();
</script>

{#if state}
  <section
    class="mirror-status"
    class:rejected={state.rejected}
    role="status"
    aria-label="Rosen mirror status"
  >
    <strong
      >Rosen mirror {state.rejected
        ? 'rejected · needs a person'
        : 'failing · retrying'}</strong
    >
    {#if state.retry_at_ms}
      <span
        >Next retry: {new Date(
          Number(state.retry_at_ms),
        ).toLocaleString()}</span
      >
    {/if}
    <pre>{state.error}</pre>
    <span
      >Last mirrored commit: <code
        >{state.last_pushed_commit ?? 'no accepted commit recorded'}</code
      ></span
    >
  </section>
{/if}

<style>
  .mirror-status {
    padding: var(--worktrees-drive-padding);
    border-bottom: var(--worktrees-head-border);
    background: var(--surface);
    color: var(--warn);
    font-size: var(--reference-body-font-size);
    line-height: var(--reference-body-line-height);
    overflow-wrap: anywhere;
  }
  .mirror-status.rejected {
    color: var(--err);
  }
  strong,
  span {
    display: block;
  }
  pre {
    margin: var(--worktrees-zero);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--text-2);
  }
  pre,
  code {
    font-family: var(--mono);
  }
</style>
