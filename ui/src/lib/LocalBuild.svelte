<script lang="ts">
  import {
    localBuildCommands,
    onLocalBuildStatus,
    type LocalBuildOffer,
    type LocalBuildView,
  } from './ipc';

  // Velocity on its own landed fixes (F1 Local builds): Rust builds and decides what is
  // offered; this shows it. Nothing renders on a copy with no local build.
  let build = $state<LocalBuildView | null>(null);

  $effect(() => {
    let stopped = false;
    void localBuildCommands.status().then((view) => {
      if (!stopped) build = view;
    });
    const listening = onLocalBuildStatus(
      (event) => (build = event.payload.view),
    );
    return () => {
      stopped = true;
      void listening.then((unlisten) => unlisten());
    };
  });

  // The result arrives on `local-build://status`; a failure shows in the status line.
  const restart = (offer: LocalBuildOffer) =>
    void localBuildCommands.restart(offer.commit);
</script>

{#if build && (build.restart || build.previous || build.status)}
  <div
    class="local-build"
    role="group"
    aria-label="Local build"
    data-tauri-drag-region
  >
    {#if build.status}<span
        class="local-build-status"
        class:failed={build.failed}
        role="status"
        title={build.detail || build.status}>{build.status}</span
      >{/if}
    {#if build.restart}{@const offer = build.restart}<button
        class="mini primary"
        title={offer.title}
        disabled={build.busy}
        onclick={() => restart(offer)}>{offer.label}</button
      >{/if}
    {#if build.previous}{@const offer = build.previous}<button
        class="mini ghost"
        title={offer.title}
        disabled={build.busy}
        onclick={() => restart(offer)}>{offer.label}</button
      >{/if}
  </div>
{/if}
