<script lang="ts">
  import type { LayoutSnapshot } from './ipc';

  // The project's Rosen server still runs another build than this Velocity's
  // because it could not be replaced at start (#515). Rust words both lines.
  let { notice }: { notice: LayoutSnapshot['rosen_server_notice'] } = $props();
</script>

{#if notice}
  <section
    class="server-status"
    role="status"
    aria-label="Rosen server version"
  >
    <strong>{notice.headline}</strong>
    <pre>{notice.reason}</pre>
  </section>
{/if}

<style>
  .server-status {
    padding: var(--worktrees-drive-padding);
    border-bottom: var(--worktrees-head-border);
    background: var(--surface);
    color: var(--warn);
    font-size: var(--reference-body-font-size);
    line-height: var(--reference-body-line-height);
    overflow-wrap: anywhere;
  }
  strong {
    display: block;
  }
  pre {
    margin: var(--worktrees-zero);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--text-2);
    font-family: var(--mono);
  }
</style>
