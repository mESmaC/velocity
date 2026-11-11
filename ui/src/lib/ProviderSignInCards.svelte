<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import {
    onProviders,
    providerCommands,
    providerIcon,
    type LayoutAction,
    type ProviderKind,
    type SignInCardView,
  } from './ipc';

  let {
    send,
    report,
  }: {
    send: (action: LayoutAction) => void;
    report: (reason: unknown) => void;
  } = $props();
  let cards = $state<SignInCardView[]>([]);

  function signIn(provider: ProviderKind) {
    send({ Request: { Navigation: 'Dismiss' } });
    send({ Request: { SettingsTab: 'providers' } });
    void providerCommands.signIn(provider).catch(report);
  }

  onMount(() => {
    let active = true;
    let stop: (() => void) | undefined;
    async function start() {
      const unlisten = await onProviders((event) => {
        if (active) cards = event.payload.view.defaults.sign_in;
      });
      if (!active) {
        unlisten();
        return;
      }
      stop = unlisten;
      const snapshot = await providerCommands.list();
      if (active) cards = snapshot.defaults.sign_in;
    }
    void start().catch(report);
    return () => {
      active = false;
      stop?.();
    };
  });
</script>

{#each cards as card (card.provider)}
  <button
    type="button"
    class="model-card out"
    disabled={!card.can_sign_in}
    onclick={() => signIn(card.provider)}
  >
    <span class="mh"
      ><Icon name={providerIcon(card.provider)} />{card.name}</span
    >
    <span class="md">{card.note}</span>
  </button>
{/each}
