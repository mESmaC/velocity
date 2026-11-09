<script lang="ts">
  import Icon from './Icon.svelte';
  import SlashMenu from './SlashMenu.svelte';
  import type {
    LayoutAction,
    LayoutSnapshot,
    ModelPillView,
    ProviderColor,
  } from './ipc';
  let {
    layout,
    send,
    id,
    placeholder,
    modelPill,
    showWorktreePill = false,
  }: {
    layout: LayoutSnapshot;
    send: (action: LayoutAction) => void;
    id: string;
    placeholder: string;
    modelPill: ModelPillView;
    showWorktreePill?: boolean;
  } = $props();

  const providerIcons: Record<ProviderColor, string> = {
    Claude: 'claude',
    Codex: 'codex',
    Antigravity: 'antigravity',
    Kimi: 'kimi',
  };

  function handleKeydown(event: KeyboardEvent) {
    const slashOpen = layout.slash_items.length > 0;
    if (slashOpen && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
      event.preventDefault();
      send(event.key === 'ArrowDown' ? 'SlashNext' : 'SlashPrevious');
      return;
    }
    if (slashOpen && (event.key === 'Tab' || event.key === 'Enter')) {
      event.preventDefault();
      send('SlashComplete');
      return;
    }
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      send('SubmitComposer');
      return;
    }
    if (event.key === 'Escape' && event.currentTarget instanceof HTMLElement) {
      event.currentTarget.blur();
    }
  }
</script>

<form
  class="composer"
  onsubmit={(event) => {
    event.preventDefault();
    send('SubmitComposer');
  }}
>
  <SlashMenu
    items={layout.slash_items}
    selected={layout.slash_selection}
    {send}
  />
  <textarea
    {id}
    aria-label="Prompt"
    {placeholder}
    spellcheck="false"
    value={layout.composer_text}
    oninput={(event) => send({ ComposerText: event.currentTarget.value })}
    onkeydown={handleKeydown}></textarea>
  <div class="composer-row">
    <button class="pill" type="button" title="Attach" aria-label="Attach"
      ><Icon name="attach" /></button
    >
    {#if showWorktreePill}
      <button
        class="pill"
        type="button"
        title="Worktree for this thread"
        onclick={() => send({ Request: { SettingsTab: 'worktrees' } })}
        ><Icon name="branch" /><i>new worktree</i></button
      >
    {/if}
    <span class="spacer"></span>
    <button
      class="pill model"
      type="button"
      title="Choose model"
      onclick={() => send({ Request: { SettingsTab: 'providers' } })}
    >
      <span class="prov" data-provider={modelPill.provider}
        ><Icon name={providerIcons[modelPill.provider]} /></span
      >
      <b>{modelPill.model_name}</b>
      <i>{modelPill.effort_label}{modelPill.show_fast_badge ? ' · Fast' : ''}</i
      >
    </button>
    <button class="pill" type="button" title="Dictate" aria-label="Dictate"
      ><Icon name="microphone" /></button
    >
    <button
      class="send"
      type="submit"
      aria-label="Send prompt"
      title="Send (Enter)"><Icon name="arrow" /></button
    >
  </div>
</form>
