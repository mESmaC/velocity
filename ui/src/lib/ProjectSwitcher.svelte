<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Icon from './Icon.svelte';
  import {
    newProjectCommands,
    onLayoutIntent,
    onProjectSelected,
    openProjectCommands,
    projectCommands,
  } from './ipc';
  import type {
    LayoutAction,
    OpenProjectRequest,
    ProjectList,
    RepositoryPickerView,
  } from './ipc';
  import './theme/projects.css';

  let {
    name,
    send,
    select,
  }: {
    name: string;
    send: (action: LayoutAction) => void;
    select: (identifier: number) => void;
  } = $props();

  let menu: HTMLElement;
  let projects = $state<ProjectList | null>(null);
  let error = $state('');
  let generation = 0;
  let active = false;
  // Open from GitHub swaps the project list for a repository picker in the same menu.
  let showing = $state<'projects' | 'github'>('projects');
  let query = $state('');
  let cloneTo = $state('');
  let picker = $state<RepositoryPickerView | null>(null);
  let reading = $state(false);
  let opening = $state('');
  let search = $state<HTMLInputElement | null>(null);
  let searchGeneration = 0;

  function report(reason: unknown) {
    if (active)
      error = reason instanceof Error ? reason.message : String(reason);
  }

  async function refresh(focus = false) {
    const request = ++generation;
    const result = await projectCommands.list();
    if (!active || request !== generation) return;
    projects = result;
    error = '';
    if (focus) {
      await tick();
      if (active && request === generation && menu.matches(':popover-open'))
        (
          menu.querySelector<HTMLElement>('[aria-current="true"]') ??
          menu.querySelector<HTMLElement>('button')
        )?.focus();
    }
  }

  // Only DOM focus moves here. Project membership, active selection, and the
  // project view are returned by Rust; popover dismissal is native presentation.
  function focusKey(event: KeyboardEvent) {
    const keys = ['ArrowDown', 'ArrowUp', 'Home', 'End'];
    if (!keys.includes(event.key)) return;
    // Home and End move the caret while you type in the search or clone folder.
    const typing = event.target instanceof HTMLInputElement;
    if (typing && (event.key === 'Home' || event.key === 'End')) return;
    event.preventDefault();
    event.stopPropagation();
    const buttons = Array.from(menu.querySelectorAll<HTMLElement>('button'));
    const current = buttons.findIndex(
      (button) => button === document.activeElement,
    );
    const next =
      event.key === 'Home'
        ? 0
        : event.key === 'End'
          ? buttons.length - 1
          : (current + (event.key === 'ArrowDown' ? 1 : buttons.length - 1)) %
            buttons.length;
    buttons[next]?.focus();
  }

  function newProject() {
    menu.hidePopover();
    send({ Request: { Navigation: 'NewProject' } });
  }

  async function open(request: OpenProjectRequest, label: string) {
    error = '';
    opening = label;
    try {
      await openProjectCommands.open(request);
      showing = 'projects';
    } catch (reason) {
      report(reason);
    } finally {
      if (active) opening = '';
    }
  }

  async function openFolder() {
    menu.hidePopover();
    error = '';
    try {
      const path = await newProjectCommands.pickFolder();
      if (path) await open({ LocalFolder: { path } }, path);
    } catch (reason) {
      report(reason);
    }
  }

  async function searchRepositories(refresh: boolean) {
    const request = ++searchGeneration;
    reading = picker === null;
    try {
      const result = await openProjectCommands.repositories(query, refresh);
      if (active && request === searchGeneration) picker = result;
    } catch (reason) {
      report(reason);
    } finally {
      if (active && request === searchGeneration) reading = false;
    }
  }

  async function showGitHub() {
    showing = 'github';
    error = '';
    await tick();
    search?.focus();
    if (!cloneTo) {
      void newProjectCommands
        .defaults()
        .then((defaults) => {
          if (active && !cloneTo) cloneTo = defaults.clone_to;
        })
        .catch(report);
    }
    // Show the list read earlier at once, then read it again for new repositories.
    await searchRepositories(false);
    await searchRepositories(true);
  }

  function openRepository(repository: string) {
    void open(
      { GitHub: { repository, clone_to: cloneTo } },
      `Opening ${repository}…`,
    );
  }

  function openFirstMatch(event: KeyboardEvent) {
    if (event.key !== 'Enter') return;
    event.preventDefault();
    const first = picker?.rows[0]?.full_name ?? picker?.typed;
    if (first) openRepository(first);
  }

  onMount(() => {
    active = true;
    const stops: (() => void)[] = [];
    async function start() {
      const stopProjects = await onProjectSelected((event) => {
        if (!active) return;
        generation += 1;
        projects = event.payload.projects;
        error = '';
        if (!event.payload.selection.menu_open && menu.matches(':popover-open'))
          menu.hidePopover();
      });
      if (!active) {
        stopProjects();
        return;
      }
      stops.push(stopProjects);
      const stopIntent = await onLayoutIntent((event) => {
        const intent = event.payload.intent;
        if (
          active &&
          typeof intent !== 'string' &&
          'Navigation' in intent &&
          intent.Navigation === 'NewProject' &&
          menu.matches(':popover-open')
        )
          menu.hidePopover();
      });
      if (!active) {
        stopIntent();
        return;
      }
      stops.push(stopIntent);
      await refresh();
    }
    void start().catch(report);
    return () => {
      active = false;
      generation += 1;
      stops.forEach((stop) => stop());
    };
  });
</script>

<button
  class="project-select"
  title="Switch project"
  aria-haspopup="menu"
  popovertarget="project-menu"
  >{name || 'Open project'}<Icon name="chevron" /></button
>
<div
  bind:this={menu}
  id="project-menu"
  class="project-menu"
  class:github={showing === 'github'}
  popover="auto"
  role="menu"
  aria-label="Switch project"
  tabindex="-1"
  onkeydown={focusKey}
  ontoggle={(event) => {
    if (event.newState === 'open') void refresh(true).catch(report);
    else showing = 'projects';
  }}
>
  {#if showing === 'github'}
    <div class="menu-head">
      <button
        role="menuitem"
        onclick={async () => {
          showing = 'projects';
          await tick();
          menu.querySelector<HTMLElement>('button')?.focus();
        }}>‹ Back</button
      >Your repositories
    </div>
    <label class="inp"
      ><input
        bind:this={search}
        bind:value={query}
        placeholder="Search, or type owner/name"
        spellcheck="false"
        aria-label="Search your repositories"
        oninput={() => void searchRepositories(false)}
        onkeydown={openFirstMatch}
      /></label
    >
    <div class="repository-list">
      {#each picker?.rows ?? [] as repository (repository.full_name)}
        <button
          role="menuitem"
          title={repository.description ?? repository.full_name}
          disabled={opening !== ''}
          onclick={() => openRepository(repository.full_name)}
          ><span class="repository-name">{repository.full_name}</span><span
            class="sub">{repository.private ? 'private' : 'public'}</span
          ></button
        >
      {/each}
      {#if picker?.typed}
        <button
          role="menuitem"
          disabled={opening !== ''}
          onclick={() => picker?.typed && openRepository(picker.typed)}
          ><span class="repository-name">Open {picker.typed}</span></button
        >
      {/if}
      {#if opening}<p class="menu-note" role="status">{opening}</p>
      {:else if reading}<p class="menu-note" role="status">
          Reading your repositories…
        </p>
      {:else if picker && picker.rows.length === 0 && !picker.typed}<p
          class="menu-note"
          role="status"
        >
          No repositories match.
        </p>
      {:else if picker && picker.hidden > 0}<p class="menu-note">
          {picker.hidden} more · keep typing to narrow the list
        </p>{/if}
    </div>
    <div class="clone-row">
      <span>Clone to</span><label class="inp"
        ><input
          bind:value={cloneTo}
          spellcheck="false"
          aria-label="Clone to folder"
        /></label
      >
    </div>
  {:else}
    {#each projects?.rows ?? [] as project, index (`${index}:${project.identifier}`)}
      <button
        role="menuitem"
        class:cur={project.selected}
        aria-current={project.selected ? 'true' : undefined}
        title={project.repository_path}
        onclick={() => {
          error = '';
          select(project.identifier);
        }}
      >
        {project.name}<span class="sub">{project.label}</span>
      </button>
    {:else}<p class="empty" role="status">No projects yet.</p>{/each}
    <hr />
    <button role="menuitem" onclick={newProject}
      ><Icon name="plus" />New project…<span class="sub">Ctrl⇧N</span></button
    >
    <button role="menuitem" onclick={() => void openFolder()}
      ><Icon name="folder" />Open folder…</button
    >
    <button role="menuitem" onclick={() => void showGitHub()}
      ><Icon name="github" />Open from GitHub…</button
    >
    {#if opening}<p class="menu-note" role="status">{opening}</p>{/if}
  {/if}
</div>
{#if error}<p class="layout-error" role="alert">{error}</p>{/if}
