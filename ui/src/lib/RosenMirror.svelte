<script lang="ts">
  import { rosenMirrorCommands } from './ipc';
  import type { MirrorDivergence, MirrorState } from './ipc';

  // `state` is renamed here: the name would shadow the `$state` rune.
  let {
    state: mirror,
    divergence = null,
    project = null,
  }: {
    state: MirrorState | null;
    // The base and mirror diverged at the last catch-up (#501); Rust keeps it.
    // Only the project header shows it; Settings shows push failures alone.
    divergence?: MirrorDivergence | null;
    project?: number | null;
  } = $props();

  let running = $state('');
  let error = $state('');

  // Rosen answers with a toast; a refusal stays here beside the divergence.
  async function run(
    label: string,
    action: (project: number) => Promise<null>,
  ) {
    if (project === null || running) return;
    running = label;
    error = '';
    try {
      await action(project);
    } catch (reason) {
      error = reason instanceof Error ? reason.message : String(reason);
    } finally {
      running = '';
    }
  }

  function short(commit: string) {
    return commit.slice(0, 12);
  }
</script>

{#if divergence}
  <section
    class="mirror-status rejected"
    role="status"
    aria-label="Rosen mirror divergence"
  >
    <strong>Rosen base and mirror have diverged · needs a person</strong>
    <span
      >Rosen base <code title={divergence.base_commit}
        >{short(divergence.base_commit)}</code
      >
      · mirror tip
      <code title={divergence.mirror_commit}
        >{short(divergence.mirror_commit)}</code
      ></span
    >
    <pre>{divergence.detail}</pre>
    {#if project !== null}
      <div class="mirror-actions">
        <button
          class="mini"
          disabled={running !== ''}
          title="For a mirror whose edits Rosen already holds; Rosen refuses otherwise"
          onclick={() => void run('Adopting…', rosenMirrorCommands.adopt)}
          >Adopt mirror tip</button
        >
        <button
          class="mini"
          disabled={running !== ''}
          onclick={() => void run('Catching up…', rosenMirrorCommands.catchUp)}
          >Catch up again</button
        >
        {#if running}<span role="status">{running}</span>{/if}
      </div>
    {/if}
    {#if error}<p role="alert">{error}</p>{/if}
  </section>
{/if}
{#if mirror}
  <section
    class="mirror-status"
    class:rejected={mirror.rejected}
    role="status"
    aria-label="Rosen mirror status"
  >
    <strong
      >Rosen mirror {mirror.rejected
        ? 'rejected · needs a person'
        : 'failing · retrying'}</strong
    >
    {#if mirror.retry_at_ms}
      <span
        >Next retry: {new Date(
          Number(mirror.retry_at_ms),
        ).toLocaleString()}</span
      >
    {/if}
    <pre>{mirror.error}</pre>
    <span
      >Last mirrored commit: <code
        >{mirror.last_pushed_commit ?? 'no accepted commit recorded'}</code
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
  .mirror-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-3);
    margin-top: var(--sp-3);
  }
  .mirror-actions span {
    color: var(--text-2);
  }
  p {
    margin: var(--worktrees-zero);
  }
</style>
