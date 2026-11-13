<script lang="ts">
  import { onMount } from 'svelte';
  import { pop } from './motion';
  import { updateCommands, type UpdateView } from './ipc';
  import './theme/usage.css';
  import './theme/updates.css';

  // The Updates modal (SPEC.md F1). The reference UI mocks no updates view, so it reuses
  // the usage modal's frame and rows. Rust writes every line and decides the one action;
  // the modal shows `update://status` as it arrives.
  let {
    update,
    close,
  }: {
    update: UpdateView;
    close: () => void;
  } = $props();

  let dialog: HTMLDialogElement;

  onMount(() => {
    const previous = document.activeElement;
    dialog.showModal();
    return () => {
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus();
    };
  });

  // Results arrive on `update://status`; a failure shows in the modal, not as an error.
  function act() {
    if (update.action === 'check') void updateCommands.check();
    else if (update.action === 'install') void updateCommands.install();
  }
</script>

<dialog
  bind:this={dialog}
  in:pop|global
  class="usage-dialog"
  aria-labelledby="updates-title"
  oncancel={(event) => {
    event.preventDefault();
    close();
  }}
  onclick={(event) => {
    if (event.target === dialog) close();
  }}
>
  <div class="usage-modal">
    <div class="usage-modal-head">
      <h2 id="updates-title">Updates</h2>
      <span class="label">Esc to close</span>
    </div>
    <div class="usage-modal-body">
      <section class="usage-provider" aria-label="Velocity version">
        <div class="usage-provider-head">
          {update.headline}<span>{update.install_label}</span>
        </div>
        {#if update.detail}<p class="usage-note">{update.detail}</p>{/if}
        {#if update.progress !== null}<div class="usage-window">
            <div class="usage-window-line">
              <span
                class="usage-bar updates-progress"
                role="progressbar"
                aria-valuenow={update.progress}
                aria-valuemin={0}
                aria-valuemax={100}
                ><span style={`--usage-left: ${update.progress}%`}></span></span
              >
              <b>{update.progress}%</b>
            </div>
          </div>{/if}
        {#if update.warning}<p class="usage-note">{update.warning}</p>{/if}
      </section>
      {#if update.notes}<hr />
        <section aria-label="Release notes">
          <div class="usage-title">Release notes</div>
          <p class="usage-note updates-notes">{update.notes}</p>
        </section>{/if}
      {#if update.action !== 'none'}<hr />
        <div class="updates-foot">
          <button class="usage-button" onclick={act}
            >{update.action_label}</button
          >
        </div>{/if}
    </div>
  </div>
</dialog>
