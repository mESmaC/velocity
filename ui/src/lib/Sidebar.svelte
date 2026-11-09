<script lang="ts">
  import Icon from './Icon.svelte';
  import type { LayoutAction, LayoutSnapshot } from './ipc';
  let {
    layout,
    send,
    openMenu,
  }: {
    layout: LayoutSnapshot;
    send: (action: LayoutAction) => void;
    openMenu: (identifier: string, x: number, y: number) => void;
  } = $props();
  const icons: Record<string, string> = {
    Draft: 'new',
    Duty: 'scout',
    Swarm: 'swarm',
    Spec: 'issue',
    Working: 'circle',
    Review: 'pr',
    Error: 'alert',
    Closed: 'circle',
    Done: 'check',
    Merged: 'check',
  };
</script>

<aside class="sidebar" aria-label="Threads">
  <div class="search-row">
    <label class="search"
      ><Icon name="search" />
      <input
        id="thread-search"
        aria-label="Search threads"
        placeholder="Search"
        spellcheck="false"
        value={layout.search}
        oninput={(event) => send({ SidebarSearch: event.currentTarget.value })}
      />
      <kbd>/</kbd>
    </label>
    <button
      class="icon-btn"
      title="New thread (Ctrl+N)"
      aria-label="New thread"
      onclick={() => send({ Request: { Navigation: 'NewThread' } })}
      ><Icon name="new" /></button
    >
  </div>
  <nav class="thread-groups" aria-label="Thread groups">
    {#each layout.groups as group (group.group)}
      <section aria-label={group.title || 'Drafts'}>
        {#if group.title}<div class="group-h">
            <span>{group.title}</span>
            {#if group.group === 'Duty'}<button
                class="icon-btn"
                title="Add duty agents"
                aria-label="Add duty agents"
                onclick={() => send({ Request: { Navigation: 'PrDuty' } })}
                ><Icon name="plus" /></button
              >{:else}<span>{group.count}</span>{/if}
          </div>{/if}
        {#if group.group === 'Duty' && group.rows.length === 0}
          <button
            class="duty-empty"
            onclick={() => send({ Request: { Navigation: 'PrDuty' } })}
            >No agents on duty · start one</button
          >
        {/if}
        {#each group.rows as row (row.identifier)}
          <div class="thread-wrap" class:member={row.member}>
            <button
              class="thread"
              class:active={row.active}
              class:paused={row.paused}
              data-status={row.group}
              aria-current={row.active ? 'page' : undefined}
              title={row.title}
              onclick={() => send({ OpenThread: row.identifier })}
              oncontextmenu={(event) => {
                event.preventDefault();
                openMenu(row.identifier, event.clientX, event.clientY);
              }}
            >
              {#if layout.show_numbers && row.number}<span class="t-key"
                  >{row.number}</span
                >{/if}
              <span class="status-icon"><Icon name={icons[row.group]} /></span>
              <span class="t-title" class:titling={row.titling}
                >{row.title}</span
              >
              {#if row.agents !== '0'}<span class="agents-count"
                  >×{row.agents}</span
                >{/if}
              {#if row.metadata}<span class="t-meta">{row.metadata}</span>{/if}
            </button>
          </div>
        {/each}
      </section>
    {/each}
    {#if layout.groups.length === 0}<p class="empty">
        No matching threads.
      </p>{/if}
  </nav>
  <footer class="sidebar-foot">
    <button
      class="icon-btn"
      title="Settings (Ctrl+,)"
      aria-label="Settings"
      onclick={() => send({ Request: { Navigation: 'Settings' } })}
      ><Icon name="settings" /></button
    >
    <button
      class="icon-btn"
      title="Usage"
      aria-label="Usage"
      onclick={() => send({ Request: 'Usage' })}><Icon name="usage" /></button
    >
    <button
      class="icon-btn"
      title="Updates"
      aria-label="Updates"
      onclick={() => send({ Request: 'Updates' })}
      ><Icon name="update" /></button
    >
    <span class="spacer"></span>
    <button
      class="worktree-indicator"
      title="Settings · Worktrees"
      onclick={() => send({ Request: { SettingsTab: 'worktrees' } })}
      ><Icon name="folder" />{layout.worktree_label}</button
    >
    {#if layout.providers.length}<button
        class="provider-limits"
        title="Subscription limits · Settings · Providers"
        aria-label="Provider subscription limits"
        onclick={() => send({ Request: { SettingsTab: 'providers' } })}
      >
        {#each layout.providers as provider (provider.provider)}<span
            class="provider-meter"
            data-provider={provider.provider}
            title={`${provider.label}: ${provider.percent_used}% used`}
            ><span style={`--provider-used: ${provider.percent_used}%`}
            ></span></span
          >{/each}
      </button>{/if}
  </footer>
</aside>
