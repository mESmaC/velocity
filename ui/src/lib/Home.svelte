<script lang="ts">
  import Composer from './Composer.svelte';
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
    { label: 'Goal swarm', key: 'G', icon: 'goal', intent: 'Goal' },
    { label: 'Dispatch an issue', key: 'I', icon: 'issue', intent: 'Dispatch' },
    { label: 'Resume last thread', key: 'R', icon: 'play', intent: 'Resume' },
    { label: 'Fan out ×3', key: 'F', icon: 'swarm', intent: 'Fanout' },
    { label: 'Start review agents', key: 'S', icon: 'scout', intent: 'Scouts' },
    { label: 'Swarm the Task List', key: 'W', icon: 'swarm', intent: 'Swarm' },
  ];
</script>

<div class="home-view">
  <Composer
    {layout}
    {send}
    id="home-composer"
    placeholder="Ask anything. @ files, $ skills, / commands"
    modelPill={layout.composer_model_pill}
    showWorktreePill={true}
    draftContext={layout.draft_context}
    threadIdentifier={null}
  />
  <div class="starters">
    {#each starters as starter (starter.intent)}<button
        class="chip"
        disabled={layout.blocked_navigation.includes(starter.intent)}
        title={layout.blocked_navigation.includes(starter.intent)
          ? (layout.build_block_reason ?? undefined)
          : undefined}
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
