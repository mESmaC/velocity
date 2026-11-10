<script lang="ts">
  import { tick } from 'svelte';
  import Icon from './Icon.svelte';
  import PullRequestPanel from './PullRequestPanel.svelte';
  import IssuePanel from './IssuePanel.svelte';
  import type { LayoutAction, LayoutSnapshot } from './ipc';
  let {
    layout,
    send,
  }: { layout: LayoutSnapshot; send: (action: LayoutAction) => void } =
    $props();
  let taskList: globalThis.HTMLDivElement;
  let lastProject = $state<number | null>(null);
  let lastTask = $state<string | undefined>();
  $effect(() => {
    const project = layout.project_identifier;
    const newest = layout.tasks.at(-1)?.identifier;
    if (project === lastProject && newest && newest !== lastTask) {
      void tick().then(() =>
        taskList?.lastElementChild?.scrollIntoView({ block: 'nearest' }),
      );
    }
    lastProject = project;
    lastTask = newest;
  });
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
        <button
          aria-label="Add a task"
          title="Add a task (T)"
          onclick={() => send({ Request: { Navigation: 'Task' } })}
          ><Icon name="plus" /></button
        >
      </div>
    </div>
    <div class="panel-body task-body">
      <div class="task-items" bind:this={taskList}>
        {#each layout.tasks as task (task.identifier)}
          <div
            class="item task"
            class:done={task.state === 'done'}
            class:now={task.state === 'now'}
            title={task.title}
          >
            <Icon
              name={task.state === 'draft'
                ? 'dashed'
                : task.state === 'done'
                  ? 'check'
                  : task.state === 'now'
                    ? 'play'
                    : 'circle'}
            />
            <span class="item-title">{task.title}</span>
            {#if task.metadata || task.attribution}
              <div class="item-meta task-sources">
                {#if task.thread_identifier}
                  <button
                    class="task-source"
                    onclick={() =>
                      task.thread_identifier &&
                      send({ OpenThread: task.thread_identifier })}
                    >{task.metadata}</button
                  >
                {/if}
                {#if task.attribution_thread}
                  <button
                    class="task-source"
                    onclick={() =>
                      task.attribution_thread &&
                      send({ OpenThread: task.attribution_thread })}
                    >{task.attribution}</button
                  >
                {:else if task.attribution}<span>{task.attribution}</span>{/if}
              </div>
            {/if}
          </div>
        {:else}<p class="empty">
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
