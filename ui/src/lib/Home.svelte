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
  <Composer
    {layout}
    {send}
    id="home-composer"
    placeholder="Ask anything. @ files, $ skills, / commands"
    modelPill={layout.default_model_pill}
    showWorktreePill={true}
  />
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
