<script lang="ts">
  import { onMount } from 'svelte';
  import { worktreeCommands, onWorktreeInventory } from './ipc';
  import type {
    LayoutAction,
    WorktreeSettingsView,
    WorktreeSettingsEdit,
  } from './ipc';
  import WorktreeSettingsPanel from './WorktreeSettingsPanel.svelte';

  let { send }: { send: (action: LayoutAction) => void } = $props();
  let view = $state<WorktreeSettingsView | null>(null);
  let error = $state('');
  let notice = $state('');
  let pruning = $state(false);
  let latest: WorktreeSettingsView | null = null;
  let pending = 0;
  let queue = Promise.resolve();

  function report(reason: unknown) {
    error = reason instanceof Error ? reason.message : String(reason);
  }

  function receive(snapshot: WorktreeSettingsView) {
    if (!latest || snapshot.revision >= latest.revision) latest = snapshot;
    if (!pending) view = latest;
  }

  function edit(change: WorktreeSettingsEdit) {
    const project = view?.project_identifier ?? null;
    pending += 1;
    error = '';
    notice = '';
    queue = queue
      .then(async () => receive(await worktreeCommands.set(change, project)))
      .catch(report)
      .finally(() => {
        pending -= 1;
        if (!pending) view = latest;
      });
  }

  async function browse() {
    try {
      await queue;
      const path = await worktreeCommands.browse();
      if (path !== null) edit({ Root: path });
    } catch (reason) {
      report(reason);
    }
  }

  async function prune() {
    pruning = true;
    error = '';
    try {
      notice = await worktreeCommands.prune();
    } catch (reason) {
      report(reason);
    } finally {
      pruning = false;
    }
  }

  onMount(() => {
    let active = true;
    let stop: (() => void) | undefined;
    async function start() {
      const unlisten = await onWorktreeInventory((event) => {
        if (active) receive(event.payload.state);
      });
      if (!active) {
        unlisten();
        return;
      }
      stop = unlisten;
      const snapshot = await worktreeCommands.list();
      if (active) receive(snapshot);
    }
    void start().catch(report);
    return () => {
      active = false;
      stop?.();
    };
  });
</script>

<WorktreeSettingsPanel
  {view}
  {error}
  {notice}
  {pruning}
  {edit}
  {browse}
  {prune}
  {send}
/>
