<script lang="ts">
  import type { MessageMode, ThreadMenuAction, ThreadMenuView } from './ipc';
  import { pop, restoreOverlayFocus } from './motion';
  import { tokenPixels } from './theme/length';
  let {
    menu,
    x,
    y,
    opener,
    choose,
    chooseMode,
    close,
  }: {
    menu: ThreadMenuView;
    x: number;
    y: number;
    opener: HTMLElement;
    choose: (action: ThreadMenuAction) => void;
    chooseMode: (mode: MessageMode) => void;
    close: () => void;
  } = $props();
  let element = $state<HTMLElement>();
  let focusTarget: HTMLElement;
  const modes: { mode: MessageMode; label: string }[] = [
    { mode: 'Open', label: 'Open' },
    { mode: 'HeadsDown', label: 'Heads-down' },
    { mode: 'Quarantined', label: 'Quarantined' },
  ];
  let left = $derived(x);
  let top = $derived(y);

  // Keep the menu inside the window, as the reference's `openCtx` does.
  $effect(() => {
    focusTarget = opener;
    if (!element) return;
    const menuElement = element;
    const preferredLeft = x;
    const preferredTop = y;
    const place = () => {
      const gap = tokenPixels(menuElement, '--thread-menu-edge-gap');
      left = Math.max(
        gap,
        Math.min(
          preferredLeft,
          window.innerWidth - menuElement.offsetWidth - gap,
        ),
      );
      top = Math.max(
        gap,
        Math.min(
          preferredTop,
          window.innerHeight - menuElement.offsetHeight - gap,
        ),
      );
    };
    place();
    const observer = new window.ResizeObserver(place);
    observer.observe(menuElement);
    return () => observer.disconnect();
  });

  function outside(event: MouseEvent) {
    if (
      element &&
      !element.inert &&
      event.target instanceof window.Node &&
      !element.contains(event.target)
    )
      close();
  }

  function escape(event: KeyboardEvent) {
    if (
      event.key === 'Escape' &&
      !event.defaultPrevented &&
      !document.querySelector('dialog:modal') &&
      element &&
      !element.inert
    ) {
      event.preventDefault();
      close();
    }
  }
</script>

<svelte:document onmousedown={outside} onkeydown={escape} />
<svelte:window onblur={close} onresize={close} />

<div
  class="thread-menu"
  role="menu"
  aria-label="Thread actions"
  bind:this={element}
  style:left={`${left}px`}
  style:top={`${top}px`}
  transition:pop
  onoutroend={() => restoreOverlayFocus(focusTarget, element)}
>
  {#if menu.message_mode}
    {#each modes as option (option.mode)}
      <button
        role="menuitemradio"
        aria-checked={menu.message_mode.mode === option.mode}
        onclick={() => chooseMode(option.mode)}
        >Message mode: {option.label}{#if menu.message_mode.mode === option.mode}<small
            >Selected</small
          >{/if}</button
      >
    {/each}
    <hr />
  {/if}
  {#each menu.entries as entry, index (index)}
    {#if entry === 'Separator'}<hr />{:else}<button
        role="menuitem"
        class:danger={entry.Item.danger}
        onclick={() => choose(entry.Item.action)}
        >{entry.Item.label}{#if entry.Item.detail}<small
            >{entry.Item.detail}</small
          >{/if}</button
      >{/if}
  {/each}
</div>
