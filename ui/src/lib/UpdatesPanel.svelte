<script lang="ts">
  import { pop } from './motion';
  import { updateCommands, type UpdateView } from './ipc';
  import './theme/usage.css';
  import './theme/updates.css';

  // The footer Updates panel (SPEC.md F1). The reference UI mocks no updates panel, so it
  // reuses the usage panel's popover frame and rows. Rust writes every line and decides
  // the one action; the panel shows `update://status` as it arrives.
  let {
    update,
    close,
  }: {
    update: UpdateView;
    close: () => void;
  } = $props();

  let panel: HTMLElement | undefined = $state();

  function dismiss(event: MouseEvent) {
    const target = event.target;
    if (!(target instanceof Element) || panel?.contains(target)) return;
    // The footer button toggles the panel itself.
    if (target.closest('.updates-trigger')) return;
    close();
  }

  function escape(event: KeyboardEvent) {
    if (event.key === 'Escape') close();
  }

  // Results arrive on `update://status`; a failure shows in the panel, not as an error.
  function act() {
    if (update.action === 'check') void updateCommands.check();
    else if (update.action === 'install') void updateCommands.install();
  }
</script>

<svelte:document onmousedown={dismiss} onkeydown={escape} />

<div
  class="usage-panel"
  role="dialog"
  aria-label="Updates"
  bind:this={panel}
  in:pop
  out:pop
>
  <div class="usage-title">Updates</div>
  <section class="usage-provider">
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
    {#if update.notes}<p class="usage-note updates-notes">
        {update.notes}
      </p>{/if}
    {#if update.warning}<p class="usage-note">{update.warning}</p>{/if}
  </section>
  {#if update.action !== 'none'}<hr />
    <button class="usage-link" onclick={act}>{update.action_label}</button>{/if}
</div>
