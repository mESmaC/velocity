<script lang="ts">
  import type { LayoutAction, SlashCommandItem } from './ipc';
  let {
    items,
    selected,
    send,
  }: {
    items: SlashCommandItem[];
    selected: string;
    send: (action: LayoutAction) => void;
  } = $props();
</script>

{#if items.length > 0}
  <div class="slash-menu" role="listbox" aria-label="Slash commands">
    {#each items as item, index (item.command)}
      <button
        type="button"
        role="option"
        aria-selected={String(index) === selected}
        class:sel={String(index) === selected}
        onmouseenter={() => send({ SlashSelect: String(index) })}
        onclick={() => send('SlashComplete')}
      >
        <span class="mono">{item.command}</span>
        <span class="desc">{item.description}</span>
      </button>
    {/each}
  </div>
{/if}
