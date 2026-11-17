<script lang="ts">
  import type { LayoutAction, SlashCommandItem } from './ipc';
  import { pop, restoreOverlayFocus } from './motion';
  let {
    items,
    selected,
    dismissed,
    opener,
    send,
  }: {
    items: SlashCommandItem[];
    selected: string;
    dismissed: boolean;
    opener: HTMLTextAreaElement | undefined;
    send: (action: LayoutAction) => void;
  } = $props();
  let menu = $state<HTMLElement>();
  let shown = $state.raw<SlashCommandItem[]>([]);
  $effect(() => {
    if (items.length) shown = items;
  });

  // Keep the last list in the outgoing menu while Rust clears its items.
  const label = $derived(
    shown[0]?.command.startsWith('$') ? 'Skills' : 'Slash commands',
  );
</script>

{#if !dismissed && items.length > 0}
  <div
    class="slash-menu"
    role="listbox"
    aria-label={label}
    bind:this={menu}
    transition:pop
    onoutroend={() => restoreOverlayFocus(opener, menu)}
  >
    {#each shown as item, index (item.command)}
      <button
        type="button"
        role="option"
        aria-selected={String(index) === selected}
        class:sel={String(index) === selected}
        onmouseenter={() => send({ SlashSelect: String(index) })}
        onpointerdown={(event) => event.preventDefault()}
        onclick={() => send('SlashComplete')}
      >
        <span class="mono" title={item.command}>{item.command}</span>
        <span class="desc" title={item.description}>{item.description}</span>
      </button>
    {/each}
  </div>
{/if}
