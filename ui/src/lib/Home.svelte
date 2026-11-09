<script lang="ts">
  import Icon from './Icon.svelte';
  import type { LayoutAction, LayoutSnapshot, NavigationIntent } from './ipc';
  let {
    layout,
    send,
  }: { layout: LayoutSnapshot; send: (action: LayoutAction) => void } =
    $props();
  const starters: {
    label: string;
    key: string;
    icon: string;
    intent: NavigationIntent;
  }[] = [
    { label: 'Dispatch an issue', key: 'I', icon: 'issue', intent: 'Dispatch' },
    { label: 'Resume last thread', key: 'R', icon: 'play', intent: 'Resume' },
    { label: 'Fan out ×3', key: 'F', icon: 'swarm', intent: 'Fanout' },
    {
      label: 'Put an agent on PR duty',
      key: 'V',
      icon: 'pr',
      intent: 'PrDuty',
    },
    { label: 'Send scouts', key: 'S', icon: 'scout', intent: 'Scouts' },
    { label: 'Swarm the Task List', key: 'W', icon: 'swarm', intent: 'Swarm' },
  ];
</script>

<div class="home-view">
  <form
    class="composer"
    onsubmit={(event) => {
      event.preventDefault();
      send('SubmitComposer');
    }}
  >
    <textarea
      id="home-composer"
      aria-label="Prompt"
      placeholder="What do you want to build?"
      value={layout.composer_text}
      oninput={(event) => send({ ComposerText: event.currentTarget.value })}
    ></textarea>
    <div class="composer-row">
      <button
        class="pill"
        type="button"
        onclick={() => send({ Request: { SettingsTab: 'providers' } })}
        >Choose model <Icon name="chevron" /></button
      >
      <span class="spacer"></span>
      <button
        class="send"
        type="submit"
        aria-label="Send prompt"
        title="Send prompt"><Icon name="arrow" /></button
      >
    </div>
  </form>
  <div class="starters">
    {#each starters as starter (starter.intent)}<button
        class="chip"
        onclick={() => send({ Request: { Navigation: starter.intent } })}
        ><Icon name={starter.icon} />{starter.label}<kbd>{starter.key}</kbd
        ></button
      >{/each}
  </div>
  <div class="hints">
    <span><kbd>Ctrl K</kbd> commands</span><span
      ><kbd>Ctrl N</kbd> new thread</span
    ><span><kbd>Alt 1–9</kbd> jump</span>
  </div>
</div>
