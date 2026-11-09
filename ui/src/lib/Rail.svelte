<script lang="ts">
  import Icon from './Icon.svelte';
  import PullRequestPanel from './PullRequestPanel.svelte';
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
  <section class="panel" aria-label="Open Issues">
    <div class="panel-h">
      <span class="label">Open Issues</span>
      <div class="panel-actions">
        <span class="count">{layout.issues.length || ''}</span><button
          class="swarm-button"
          onclick={() => send({ Request: 'SwarmIssues' })}
          ><Icon name="swarm" />Swarm</button
        >
      </div>
    </div>
    <div class="panel-body">
      {#each layout.issues as issue (issue.identifier)}<div class="issue-row">
          <button
            class="item"
            onclick={() => send({ Request: { OpenIssue: issue.identifier } })}
            ><Icon name="issue" /><span class="item-title">{issue.title}</span
            ><span class="count">#{issue.identifier}</span><span
              class="item-meta">{issue.metadata}</span
            ></button
          ><button
            class="dispatch"
            aria-label={`Dispatch issue ${issue.identifier}`}
            disabled={!layout.build_allowed}
            title={layout.build_block_reason ?? 'Dispatch issue'}
            onclick={() =>
              send({ Request: { DispatchIssue: issue.identifier } })}
            ><Icon name="play" />Dispatch</button
          >
        </div>{:else}<p class="empty">No open issues.</p>{/each}
    </div>
  </section>
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
