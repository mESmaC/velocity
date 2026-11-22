<script lang="ts">
  import { modal } from './motion';
  import {
    localBuildCommands,
    onLocalBuildStatus,
    type LocalBuildOffer,
    type LocalBuildView,
  } from './ipc';
  import './theme/usage.css';

  // Velocity on its own landed fixes (F1 Local builds): Rust builds and decides what is
  // offered; this shows it. Nothing renders on a copy with no local build.
  let build = $state<LocalBuildView | null>(null);
  let dialog = $state<HTMLDialogElement | undefined>(undefined);

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
  const restart = (offer: LocalBuildOffer, rollback: boolean) =>
    void localBuildCommands.restart(offer.commit, false, rollback);

  const confirmRestart = (commit: string) =>
    void localBuildCommands.restart(commit, true, false);

  const cancelConfirm = () => void localBuildCommands.cancelRestart();
</script>

{#if build && (build.restart || build.previous || build.status || build.waiting_label)}
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
    {#if build.waiting_label}<span
        class="local-build-waiting"
        role="status"
        title={build.waiting_detail || build.waiting_label}
        >{build.waiting_label}</span
      >{/if}
    {#if build.restart}{@const offer = build.restart}<button
        class="mini primary"
        title={offer.title}
        disabled={build.busy}
        onclick={() => restart(offer, false)}>{offer.label}</button
      >{/if}
    {#if build.previous}{@const offer = build.previous}<button
        class="mini ghost"
        title={offer.title}
        disabled={build.busy}
        onclick={() => restart(offer, true)}>{offer.label}</button
      >{/if}
  </div>
{/if}

{#if build?.confirm}{@const prompt = build.confirm}
  <dialog
    bind:this={dialog}
    transition:modal|global
    class="usage-dialog"
    aria-labelledby="local-build-confirm-title"
    oncancel={(event) => {
      event.preventDefault();
      cancelConfirm();
    }}
    onclick={(event) => {
      if (event.target === dialog) cancelConfirm();
    }}
  >
    <div class="usage-modal">
      <div class="usage-modal-head">
        <h2 id="local-build-confirm-title">Confirm local build restart</h2>
        <span class="label">Esc to cancel</span>
      </div>
      <div class="usage-modal-body">
        <section class="usage-provider" aria-label="Commits">
          <div class="local-build-confirm-commits">
            <span>Running build</span><b>{prompt.running_commit}</b>
            <span>Target build</span><b>{prompt.target_commit}</b>
          </div>
          <p class="usage-note">{prompt.message}</p>
        </section>
        <div class="local-build-confirm-foot">
          <button
            class="usage-button"
            disabled={build.busy}
            onclick={cancelConfirm}>Cancel</button
          >
          <button
            class="mini primary"
            disabled={build.busy}
            onclick={() => confirmRestart(prompt.commit)}>Restart anyway</button
          >
        </div>
      </div>
    </div>
  </dialog>
{/if}
