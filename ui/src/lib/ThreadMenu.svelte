<script lang="ts">
  import type { ThreadMenuAction, ThreadMenuView } from './ipc';
  let {
    menu,
    x,
    y,
    choose,
    close,
  }: {
    menu: ThreadMenuView;
    x: number;
    y: number;
    choose: (action: ThreadMenuAction) => void;
    close: () => void;
  } = $props();
  let element = $state<HTMLElement>();
  let left = $derived(x);
  let top = $derived(y);

  // Keep the menu inside the window, as the reference's `openCtx` does.
  $effect(() => {
    if (!element) return;
    const gap = parseFloat(
      window
        .getComputedStyle(element)
        .getPropertyValue('--thread-menu-edge-gap'),
    );
    left = Math.min(x, window.innerWidth - element.offsetWidth - gap);
    top = Math.min(y, window.innerHeight - element.offsetHeight - gap);
  });

  function outside(event: MouseEvent) {
    if (!(event.target instanceof Element && element?.contains(event.target)))
      close();
  }
</script>

<svelte:window
  onmousedown={outside}
  onkeydown={(event) => {
    if (event.key === 'Escape') close();
  }}
  onblur={close}
  onresize={close}
/>

<div
  class="thread-menu"
  role="menu"
  aria-label="Thread actions"
  bind:this={element}
  style:left={`${left}px`}
  style:top={`${top}px`}
>
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
