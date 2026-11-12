<script lang="ts">
  import type { Snippet } from 'svelte';
  import { reportWindowError } from './ipc';

  // One bad value in a live update must not take the window down with it (#273). A pane
  // that throws while rendering shows a notice in its place, logs the error, and draws
  // itself again from the next snapshot Rust publishes.
  let {
    name,
    revision,
    children,
  }: {
    /** Where the error happened, for the log. */
    name: string;
    /** Changes with each snapshot; a failed pane retries when it does. */
    revision: number;
    children: Snippet;
  } = $props();

  let retry = $state<(() => void) | null>(null);
  let failedRevision = -1;

  function onerror(error: unknown, reset: () => void) {
    reportWindowError(`pane ${name}`, error);
    failedRevision = revision;
    retry = reset;
  }

  function redraw() {
    const reset = retry;
    retry = null;
    reset?.();
  }

  $effect(() => {
    if (retry && revision !== failedRevision) redraw();
  });
</script>

<svelte:boundary {onerror}>
  {@render children()}
  {#snippet failed()}
    <p class="empty" role="alert">
      Something in this pane failed to draw. It redraws with the next update.
      <button type="button" class="btn" onclick={redraw}>Redraw now</button>
    </p>
  {/snippet}
</svelte:boundary>
