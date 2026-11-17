<script lang="ts">
  import Icon from './Icon.svelte';
  import SlashMenu from './SlashMenu.svelte';
  import ModelMenu from './ModelMenu.svelte';
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
    threadIdentifier,
  }: {
    layout: LayoutSnapshot;
    send: (action: LayoutAction) => void;
    id: string;
    placeholder: string;
    modelPill: ModelPillView;
    showWorktreePill?: boolean;
    /** A thread identifier edits that thread. `null` targets the home draft. */
    threadIdentifier?: number | null;
  } = $props();

  const usage = $derived(
    layout.providers.find(
      (provider) => provider.account_key === modelPill.usage_account,
    ),
  );
  let menuOpen = $state(false);
  const canChooseModel = $derived(
    threadIdentifier !== undefined && layout.project_identifier !== null,
  );

  const providerIcons: Record<ProviderColor, string> = {
    Claude: 'claude',
    Codex: 'codex',
    Antigravity: 'antigravity',
    Kimi: 'kimi',
    Custom: 'agents',
  };

  let textarea: HTMLTextAreaElement | undefined = $state();

  // App.svelte coalesces snapshots while requests are pending, so a fast type-and-Enter
  // can commit "" right after "" and never show the typed text. Svelte's `value=` skips
  // a write equal to its last one, which left the sent text in the box (#49). Compare
  // with the live DOM value instead, on every committed snapshot. Once nothing is
  // pending, Rust's text already includes every keystroke, so this never drops typing.
  $effect(() => {
    const text = layout.composer_text;
    if (textarea && textarea.value !== text) textarea.value = text;
  });

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
    bind:this={textarea}
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
        title="Workspace for this thread"
        onclick={() => send({ Request: { SettingsTab: 'worktrees' } })}
        ><Icon name="branch" /><i>new workspace</i></button
      >
    {/if}
    <span class="spacer"></span>
    {#if menuOpen && canChooseModel && threadIdentifier !== undefined}<ModelMenu
        projectIdentifier={layout.project_identifier ?? 0}
        {threadIdentifier}
        {send}
        close={() => (menuOpen = false)}
      />{/if}
    <button
      class="pill model"
      type="button"
      title={usage?.label ?? 'Choose model'}
      aria-haspopup="menu"
      aria-expanded={menuOpen}
      disabled={!canChooseModel}
      onclick={() => (menuOpen = !menuOpen)}
    >
      <span class="prov" data-provider={modelPill.provider}
        ><Icon name={providerIcons[modelPill.provider]} /></span
      >
      <b>{modelPill.model_name}</b>
      <i>{modelPill.effort_label}{modelPill.show_fast_badge ? ' · Fast' : ''}</i
      >
    </button>
    {#if usage && !usage.note}
      <span
        class="provider-meter"
        data-provider={usage.provider}
        role="img"
        aria-label={usage.label}
        title={usage.label}
      >
        <span style={`--provider-used: ${usage.percent_used}%`}></span>
      </span>
    {/if}
    <button class="pill" type="button" title="Dictate" aria-label="Dictate"
      ><Icon name="microphone" /></button
    >
    <button
      class="send"
      type="submit"
      aria-label="Send prompt"
      disabled={!layout.composer_allowed}
      title={layout.composer_block_reason ?? 'Send (Enter)'}
      ><Icon name="arrow" /></button
    >
  </div>
</form>
