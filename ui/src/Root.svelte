<script lang="ts">
  import App from './App.svelte';
  import { reportWindowError } from './lib/ipc';

  // The last line of defence against a blank window (#273): an error no pane caught, or one
  // in the window's own frame, logs and starts the window again. A fresh App subscribes
  // again and reads the latest state from Rust, which holds everything the window shows.
  const RESTART_DELAY_MS = 2000;

  function onerror(error: unknown, reset: () => void) {
    reportWindowError('root', error);
    setTimeout(reset, RESTART_DELAY_MS);
  }
</script>

<svelte:boundary {onerror}>
  <App />
  {#snippet failed()}
    <p class="empty" role="alert">
      Something failed to draw. Reopening the window…
    </p>
  {/snippet}
</svelte:boundary>
