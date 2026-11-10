<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import {
    providerCommands,
    onProviders,
    providerIcon,
    providerMark,
  } from './ipc';
  import type {
    EffortLevel,
    LayoutAction,
    ModelMenuChoice,
    ModelMenuView,
    ProviderKind,
    SpeedMode,
  } from './ipc';
  import './theme/providers.css';

  // The composer's model menu (SPEC.md F4, F15): grouped by provider, with effort and
  // speed at the bottom. Rust draws it; a pick on a running thread applies from its next
  // message.
  let {
    projectIdentifier,
    threadIdentifier,
    send,
    close,
  }: {
    projectIdentifier: number;
    threadIdentifier: number | null;
    send: (action: LayoutAction) => void;
    close: () => void;
  } = $props();

  let view = $state<ModelMenuView | null>(null);
  let error = $state('');
  let menu: HTMLElement | undefined = $state();

  const efforts: EffortLevel[] = ['low', 'medium', 'high', 'max'];
  const effortNames: Record<EffortLevel, string> = {
    low: 'Low',
    medium: 'Medium',
    high: 'High',
    max: 'Max',
  };
  const speeds: SpeedMode[] = ['regular', 'fast'];

  function report(reason: unknown) {
    error = reason instanceof Error ? reason.message : String(reason);
  }

  function refresh() {
    providerCommands
      .menu(projectIdentifier, threadIdentifier)
      .then((next) => (view = next))
      .catch(report);
  }

  function choose(choice: ModelMenuChoice, then?: () => void) {
    error = '';
    providerCommands
      .choose(projectIdentifier, threadIdentifier, choice)
      .then((next) => {
        view = next;
        then?.();
      })
      .catch(report);
  }

  function signIn(provider: ProviderKind) {
    close();
    send({ Request: { SettingsTab: 'providers' } });
    void providerCommands.signIn(provider).catch(report);
  }

  function dismiss(event: MouseEvent) {
    const target = event.target;
    if (!(target instanceof Element) || menu?.contains(target)) return;
    if (target.closest('.pill.model')) return;
    close();
  }

  function escape(event: KeyboardEvent) {
    if (event.key === 'Escape') close();
  }

  onMount(() => {
    refresh();
    let active = true;
    let stop: (() => void) | undefined;
    void onProviders(() => {
      if (active) refresh();
    })
      .then((unlisten) => {
        if (active) stop = unlisten;
        else unlisten();
      })
      .catch(report);
    return () => {
      active = false;
      stop?.();
    };
  });
</script>

<svelte:document onmousedown={dismiss} onkeydown={escape} />

<div class="model-menu" role="menu" bind:this={menu}>
  {#if view}
    {#each view.groups as group (group.provider)}
      <div class="model-menu-head" data-provider={providerMark(group.provider)}>
        <Icon name={providerIcon(group.provider)} />{group.name}<span
          >{group.note}</span
        >
      </div>
      {#if group.signed_in}
        {#each group.models as item (item.model)}<button
            class="model-menu-item"
            class:on={item.selected}
            role="menuitemradio"
            aria-checked={item.selected}
            disabled={!item.enabled}
            title={item.enabled ? undefined : 'Start a new thread for this one'}
            onclick={() =>
              choose(
                { Model: { provider: group.provider, model: item.model } },
                close,
              )}>{item.model}</button
          >{/each}
      {:else if group.can_sign_in}<button
          class="model-menu-item sign-in"
          role="menuitem"
          onclick={() => signIn(group.provider)}
          >{group.sign_in_label}<small>OAuth in your browser</small></button
        >{/if}
    {/each}
    <hr />
    <div class="model-menu-row">
      Effort
      <span class="pv-seg">
        {#each efforts as level (level)}<button
            class:on={view.effort === level}
            onclick={() => choose({ Effort: level })}
            >{effortNames[level]}</button
          >{/each}
      </span>
    </div>
    <div class="model-menu-row">
      Speed{#if view.speed_note}<span class="model-menu-why"
          >{view.speed_note}</span
        >{/if}
      <span class="pv-seg" class:dis={!view.speed_available}>
        {#each speeds as mode (mode)}<button
            class:on={(view.speed_available ? view.speed : 'regular') === mode}
            disabled={!view.speed_available}
            onclick={() => choose({ Speed: mode })}
            >{#if mode === 'fast'}<Icon
                name="bolt"
              />Fast{:else}Regular{/if}</button
          >{/each}
      </span>
    </div>
  {:else}
    <div class="model-menu-head">Reading models…</div>
  {/if}
  {#if error}<p class="model-menu-error" role="alert">{error}</p>{/if}
</div>
