<script lang="ts">
  import Icon from './Icon.svelte';
  import { pop } from './motion';
  import type { LayoutAction, ProviderColor, ProviderUsage } from './ipc';
  import './theme/usage.css';

  // The footer usage panel (SPEC.md F15), laid out like T3 Code's usage limits: a section
  // per signed-in provider with each subscription window's share left, a bar of what is
  // left in the provider's colour, and when it resets. Rust draws every row; the panel
  // re-renders whenever `layout://state` brings new limits.
  let {
    providers,
    send,
    close,
  }: {
    providers: ProviderUsage[];
    send: (action: LayoutAction) => void;
    close: () => void;
  } = $props();

  let panel: HTMLElement | undefined = $state();

  const icons: Record<ProviderColor, string> = {
    Claude: 'claude',
    Codex: 'codex',
    Antigravity: 'antigravity',
    Kimi: 'kimi',
    Custom: 'agents',
  };

  function dismiss(event: MouseEvent) {
    const target = event.target;
    if (!(target instanceof Element) || panel?.contains(target)) return;
    // The footer buttons toggle the panel themselves.
    if (target.closest('.usage-trigger')) return;
    close();
  }

  function escape(event: KeyboardEvent) {
    if (event.key === 'Escape') close();
  }

  function openProviders() {
    close();
    send({ Request: { SettingsTab: 'providers' } });
  }
</script>

<svelte:document onmousedown={dismiss} onkeydown={escape} />

<div
  class="usage-panel"
  role="dialog"
  aria-label="Usage limits"
  bind:this={panel}
  in:pop
  out:pop
>
  <div class="usage-title">Usage limits</div>
  {#each providers as provider (provider.provider + provider.name)}
    <section class="usage-provider">
      <div class="usage-provider-head">
        <Icon
          name={icons[provider.provider]}
        />{provider.name}{#if provider.plan}<span>{provider.plan}</span>{/if}
      </div>
      {#each provider.windows as window (window.label)}
        <div class="usage-window" title={window.summary}>
          <div class="usage-window-line">
            <span class="usage-window-label">{window.label}</span>
            <span class="usage-window-reset">{window.resets_in}</span>
          </div>
          <div class="usage-window-line">
            <span
              class="usage-bar"
              role="img"
              aria-label={`${window.label}: ${window.summary}`}
              data-provider={provider.provider}
              ><span style={`--usage-left: ${window.percent_left}%`}
              ></span></span
            >
            <b>{window.percent_left}% left</b>
          </div>
        </div>
      {/each}
      {#if provider.note}<p class="usage-note">{provider.note}</p>{/if}
    </section>
  {:else}
    <p class="usage-note">No provider is signed in.</p>
  {/each}
  <hr />
  <button class="usage-link" onclick={openProviders}
    >Settings · Providers</button
  >
</div>
