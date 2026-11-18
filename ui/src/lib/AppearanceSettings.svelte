<script lang="ts">
  import SettingsNav from './SettingsNav.svelte';
  import PaneState from './PaneState.svelte';
  import type { LayoutAction, UiZoomAdjustment, UiZoomView } from './ipc';
  import './theme/worktrees.css';

  let {
    zoom,
    error,
    retry,
    adjust,
    send,
  }: {
    zoom: UiZoomView | null;
    error: string;
    retry: () => Promise<void>;
    adjust: (adjustment: UiZoomAdjustment) => void;
    send: (action: LayoutAction) => void;
  } = $props();
</script>

<div class="worktree-settings">
  <header class="worktree-settings-head"><h1>Settings</h1></header>
  <div class="worktree-settings-body">
    <SettingsNav current="appearance" {send} />
    <div class="worktree-settings-main">
      <section aria-labelledby="ui-zoom-heading">
        <h2 id="ui-zoom-heading">UI zoom</h2>
        <p class="worktree-description">
          Resize text, controls, and panes across Velocity.
        </p>
        {#if zoom}
          <div
            class="appearance-zoom"
            role="group"
            aria-labelledby="ui-zoom-heading"
          >
            <button
              class="btn"
              disabled={!zoom.can_decrease}
              onclick={() => adjust('Decrease')}
            >
              Zoom out
            </button>
            <output aria-live="polite">{zoom.percent}%</output>
            <button
              class="btn"
              disabled={!zoom.can_increase}
              onclick={() => adjust('Increase')}
            >
              Zoom in
            </button>
            <button
              class="btn"
              disabled={!zoom.can_reset}
              onclick={() => adjust('Reset')}
            >
              Reset
            </button>
          </div>
        {:else}
          <PaneState
            name="UI zoom"
            loading={!error}
            network={false}
            {error}
            {retry}
          />
        {/if}
      </section>
    </div>
  </div>
</div>
