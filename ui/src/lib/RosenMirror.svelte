<script lang="ts">
  import { collapseCommands, rosenMirrorCommands } from './ipc';
  import type {
    CollapseMirrorHealth,
    MirrorBehind,
    MirrorDivergence,
    MirrorState,
    ProjectHealthItem,
  } from './ipc';

  // `state` is renamed here: the name would shadow the `$state` rune.
  let {
    state: mirror,
    behind = null,
    divergence = null,
    project = null,
    collapse = null,
    collapseForwarding = null,
  }: {
    state: MirrorState | null;
    // The project's base trails its mirror while a catch-up is pending (#520).
    behind?: MirrorBehind | null;
    // The base and mirror diverged at the last catch-up (#501); Rust keeps it.
    // Only the project header shows it; Settings shows push failures alone.
    divergence?: MirrorDivergence | null;
    project?: number | null;
    // Collapse's forwarder to the downstream remote (#427). Only a stopped
    // forwarder or an archived repository needs the owner, so only those show.
    collapse?: CollapseMirrorHealth | null;
    // Missing or failing GitHub downstream copy surfaced in the title bar (#611).
    collapseForwarding?: ProjectHealthItem | null;
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

  function safeExternalUrl(url: string | null | undefined): string | null {
    if (!url) return null;
    try {
      const parsed = new globalThis.URL(url);
      return parsed.protocol === 'https:' ? parsed.toString() : null;
    } catch {
      return null;
    }
  }
</script>

{#if behind && !divergence}
  <section
    class="mirror-status"
    role="status"
    aria-label="Rosen mirror catch-up"
  >
    <strong
      >Behind the mirror by {behind.commits}
      {behind.commits === 1 ? 'commit' : 'commits'} · {behind.catching_up
        ? 'catching up…'
        : 'catch-up pending'}</strong
    >
    <span
      >Rosen base <code title={behind.base_commit}
        >{short(behind.base_commit)}</code
      >
      · mirror tip
      <code title={behind.mirror_commit}>{short(behind.mirror_commit)}</code
      ></span
    >
    {#if behind.error}<pre>{behind.error}</pre>{/if}
    {#if project !== null && !behind.catching_up}
      <div class="mirror-actions">
        <button
          class="mini"
          disabled={running !== ''}
          onclick={() => void run('Catching up…', rosenMirrorCommands.catchUp)}
          >Catch up now</button
        >
        {#if running}<span role="status">{running}</span>{/if}
      </div>
    {/if}
    {#if error}<p role="alert">{error}</p>{/if}
  </section>
{/if}
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
{#if collapseForwarding && !collapse?.archived}
  {@const settingsUrl = safeExternalUrl(collapseForwarding.action_url)}
  <section
    class="mirror-status rejected"
    role="status"
    aria-label="GitHub downstream copy status"
  >
    <strong>{collapseForwarding.signal}</strong>
    {#if collapseForwarding.detail}<pre>{collapseForwarding.detail}</pre>{/if}
    {#if settingsUrl}
      <div class="mirror-actions">
        <a
          class="mini"
          href={settingsUrl}
          target="_blank"
          rel="noopener noreferrer"
          title="Configure the GitHub downstream in Collapse repository settings"
          onclick={(event) => {
            event.preventDefault();
            void collapseCommands.openPage(settingsUrl);
          }}>{collapseForwarding.action_label ?? 'Open Collapse settings'} ↗</a
        >
      </div>
    {/if}
  </section>
{/if}
{#if collapse && (collapse.archived || (collapse.forwarder === 'Stopped' && !collapseForwarding))}
  <section
    class="mirror-status rejected"
    role="status"
    aria-label="Collapse forwarder status"
  >
    <strong
      >{collapse.archived
        ? 'Collapse repository archived · lands are refused'
        : 'Collapse forwarder stopped · needs a person'}</strong
    >
    {#if collapse.message}<pre>{collapse.message}</pre>{/if}
    {#if project !== null}
      <div class="mirror-actions">
        <button
          class="mini"
          title="Retry or change the downstream remote on Collapse"
          onclick={() =>
            void run('Opening…', (identifier) =>
              collapseCommands.open(identifier).then(() => null),
            )}>Open on Collapse ↗</button
        >
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
    {#if mirror.base_commit}
      <span
        >Rosen base <code title={mirror.base_commit}
          >{short(mirror.base_commit)}</code
        >
        · last mirrored commit
        <code title={mirror.last_pushed_commit ?? undefined}
          >{mirror.last_pushed_commit
            ? short(mirror.last_pushed_commit)
            : 'none'}</code
        ></span
      >
    {:else}
      <span
        >Last mirrored commit: <code
          >{mirror.last_pushed_commit ?? 'no accepted commit recorded'}</code
        ></span
      >
    {/if}
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
