<script lang="ts">
  import { onMount, tick } from 'svelte';
  import type { LayoutAction, LayoutSnapshot } from './ipc';
  let {
    layout,
    send,
  }: { layout: LayoutSnapshot; send: (action: LayoutAction) => void } =
    $props();
  let dialog: HTMLDialogElement;
  let input: HTMLInputElement;

  onMount(() => {
    const previous = document.activeElement;
    dialog.showModal();
    input.focus();
    return () => {
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus();
    };
  });

  $effect(() => {
    const selected = layout.palette_selection;
    void tick().then(() =>
      document
        .getElementById(`palette-item-${selected}`)
        ?.scrollIntoView({ block: 'nearest' }),
    );
  });

  function choose(index: number) {
    send({ PaletteSelect: String(index) });
    send('PaletteAccept');
  }
</script>

<dialog
  bind:this={dialog}
  class="palette-dialog"
  aria-label="Command palette"
  oncancel={(event) => {
    event.preventDefault();
    send({ Request: { Navigation: 'Dismiss' } });
  }}
  onclick={(event) => {
    if (event.target === dialog) send({ Request: { Navigation: 'Dismiss' } });
  }}
>
  <div class="palette">
    <input
      bind:this={input}
      role="combobox"
      aria-label="Search commands, projects, threads and issues"
      aria-controls="palette-results"
      aria-expanded="true"
      aria-autocomplete="list"
      aria-activedescendant={layout.palette_results.length
        ? `palette-item-${layout.palette_selection}`
        : undefined}
      placeholder="Type a command or search…"
      value={layout.palette_query}
      oninput={(event) => send({ PaletteSearch: event.currentTarget.value })}
    />
    <ul id="palette-results" role="listbox" aria-label="Commands">
      {#each layout.palette_results as item, index (item.identifier)}<li
          id={`palette-item-${index}`}
          role="option"
          aria-selected={String(index) === layout.palette_selection}
        >
          <button
            class:sel={String(index) === layout.palette_selection}
            tabindex="-1"
            onclick={() => choose(index)}
            >{item.label}<span class="palette-group"
              >{item.shortcut || item.group}</span
            ></button
          >
        </li>{/each}
    </ul>
    {#if layout.palette_results.length === 0}<p class="empty" role="status">
        No matching commands, projects, threads or issues.
      </p>{/if}
  </div>
</dialog>
