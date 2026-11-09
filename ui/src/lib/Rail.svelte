<script lang="ts">
  import Icon from './Icon.svelte';
  import PullRequestPanel from './PullRequestPanel.svelte';
  import IssuePanel from './IssuePanel.svelte';
  import type { LayoutAction, LayoutSnapshot } from './ipc';
  let {
    layout,
    send,
  }: { layout: LayoutSnapshot; send: (action: LayoutAction) => void } =
    $props();
</script>

<aside class="rail" aria-label="Project activity">
  <section class="panel" aria-label="Task List">
    <div class="panel-h">
      <span class="label">Task List</span>
      <div class="panel-actions">
        <span class="count">{layout.task_count_label}</span><button
          class="swarm-button"
          onclick={() => send({ Request: { Navigation: 'Swarm' } })}
          ><Icon name="swarm" />Swarm</button
        >
      </div>
    </div>
    <div class="panel-body task-body">
      <div class="task-items">
        {#each layout.tasks as task (task.identifier)}<button
            class="item"
            onclick={() => send({ Request: { OpenTask: task.identifier } })}
            ><Icon name={layout.tasks_draft ? 'dashed' : 'circle'} /><span
              class="item-title">{task.title}</span
            ><span></span><span class="item-meta">{task.metadata}</span></button
          >{:else}<p class="empty">
            Tasks show up here as the spec takes shape.
          </p>{/each}
      </div>
      <form
        class="task-add"
        onsubmit={(event) => {
          event.preventDefault();
          send('SubmitTask');
        }}
      >
        <Icon name="plus" /><input
          id="task-input"
          aria-label="Add a task"
          placeholder="Add a task…"
          value={layout.task_text}
          oninput={(event) => send({ TaskText: event.currentTarget.value })}
        /><kbd>T</kbd>
      </form>
    </div>
  </section>
  <PullRequestPanel />
  <IssuePanel {layout} {send} />
  <footer class="rail-foot">
    <button onclick={() => send({ Request: { Navigation: 'Airc' } })}
      >AIRC{#if layout.airc_unread !== '0'}<span
          class="badge"
          class:alert={layout.overlap_unread}>{layout.airc_unread}</span
        >{/if}</button
    ><button onclick={() => send({ Request: { SettingsTab: 'agents' } })}
      >AGENTS.md</button
    >
  </footer>
</aside>
