<script lang="ts">
  import { collapseCommands, type DeletedCollapseRepository } from './ipc';

  let {
    deleted,
    busy = false,
    compact = false,
    onRestore,
    onPurgeAndCreate,
  }: {
    deleted: DeletedCollapseRepository;
    busy?: boolean;
    compact?: boolean;
    onRestore: () => void;
    onPurgeAndCreate: () => void;
  } = $props();

  const formattedDeadline = $derived.by(() => {
    const raw = deleted.delete_after?.trim();
    if (!raw) return null;
    const parsed = Date.parse(raw);
    return Number.isFinite(parsed) ? new Date(parsed).toLocaleString() : raw;
  });

  const headline = $derived.by(() => {
    if (formattedDeadline) {
      return `${deleted.repository} was deleted on Collapse and is held until ${formattedDeadline}.`;
    }
    if (deleted.confirmed) {
      return `${deleted.repository} was deleted on Collapse and is held until its purge deadline.`;
    }
    return `${deleted.repository} was probably deleted on Collapse (pending purge) and is held until its 7-day purge deadline.`;
  });

  const settingsUrl = $derived.by(() => {
    try {
      const parsed = new globalThis.URL(deleted.settings_url);
      return parsed.protocol === 'https:' ? parsed.toString() : null;
    } catch {
      return null;
    }
  });

  function confirmPurge() {
    if (busy || !deleted.purge_available) return;
    if (
      !globalThis.confirm(
        `Permanently purge the deleted Collapse repository ${deleted.repository} and create a fresh empty repository? This cannot be undone.`,
      )
    ) {
      return;
    }
    onPurgeAndCreate();
  }
</script>

<div
  class="collapse-deleted-notice"
  class:compact
  role="alert"
  aria-label="Deleted Collapse repository recovery"
>
  <p class="collapse-deleted-headline">{headline}</p>
  <div class="collapse-deleted-actions">
    <button
      type="button"
      class="worktree-button mini"
      disabled={busy}
      onclick={onRestore}
    >
      {busy ? 'Working…' : 'Restore and link'}
    </button>
    {#if deleted.purge_available}
      <button
        type="button"
        class="worktree-button mini collapse-danger"
        disabled={busy}
        onclick={confirmPurge}
      >
        Purge now and create
      </button>
    {/if}
    {#if settingsUrl}
      <a
        href={settingsUrl}
        target="_blank"
        rel="noopener noreferrer"
        class="worktree-button mini collapse-deleted-link"
        onclick={(event) => {
          event.preventDefault();
          void collapseCommands.openPage(settingsUrl);
        }}
      >
        Repository settings ↗
      </a>
    {/if}
  </div>
  {#if !deleted.purge_available}
    <p class="hint collapse-deleted-hint">
      Immediate purge will be offered once Collapse supports it; restore it or
      manage it in repository settings.
    </p>
  {/if}
</div>

<style>
  .collapse-deleted-notice {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    width: 100%;
    padding: var(--sp-3) var(--sp-4);
    border: 1px solid var(--warn);
    border-radius: var(--r-sm);
    background: var(--field);
  }
  .collapse-deleted-notice.compact {
    margin-top: var(--sp-2);
    padding: var(--sp-3);
    gap: var(--sp-2);
  }
  .collapse-deleted-headline {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--text);
    overflow-wrap: anywhere;
  }
  .collapse-deleted-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
  .collapse-deleted-link {
    text-decoration: none;
  }
  .collapse-deleted-hint {
    margin: 0;
    font-size: var(--fs-xs);
    color: var(--muted);
  }
  .collapse-deleted-notice.compact :global(.worktree-button.mini) {
    width: auto;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    height: var(--h-sm);
    padding: 0 var(--sp-3);
    border: 1px solid var(--line-mid);
    border-radius: var(--r-sm);
    background: var(--surface-2);
    color: var(--text);
    font-size: var(--fs-xs);
  }
  .collapse-deleted-notice.compact :global(.collapse-danger) {
    border-color: var(--err);
    color: var(--err);
  }
</style>
