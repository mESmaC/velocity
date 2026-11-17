<script lang="ts">
  import type { ToastView } from './ipc';
  import { pop, restoreOverlayFocus } from './motion';
  let {
    toast,
    undo,
  }: { toast: ToastView | null; undo: (deletion: number) => void } = $props();
  // The reference's `toast()` hides a plain message after 2.2 s. An Undo toast stays
  // until Rust lands or undoes its deletion, which clears it from the snapshot.
  const PLAIN_TOAST_MILLISECONDS = 2200;
  let expired = $state<number | null>(null);
  // The last toast stays rendered while it fades out.
  let shown = $state<ToastView | null>(null);
  let visible = $derived(!!toast && toast.identifier !== expired);
  let element = $state<HTMLElement>();
  let opener: HTMLElement | undefined;

  function rememberFocus() {
    const active = document.activeElement;
    if (active instanceof HTMLElement && !element?.contains(active))
      opener = active;
  }

  $effect(() => {
    if (!toast) return;
    shown = toast;
    if (toast.undo_deletion !== null) return;
    const identifier = toast.identifier;
    const timer = setTimeout(
      () => (expired = identifier),
      PLAIN_TOAST_MILLISECONDS,
    );
    return () => clearTimeout(timer);
  });
</script>

{#if visible && shown}
  <div
    class="toast"
    class:act={shown.undo_deletion !== null}
    role="status"
    bind:this={element}
    transition:pop={{ style: 'slide' }}
    onintrostart={rememberFocus}
    onoutroend={() => restoreOverlayFocus(opener, element)}
  >
    <span>{shown.message}</span>
    {#if shown.undo_deletion !== null}
      <button
        class="mini"
        onclick={() => {
          if (shown?.undo_deletion != null) undo(shown.undo_deletion);
        }}>Undo</button
      >
    {/if}
  </div>
{/if}
