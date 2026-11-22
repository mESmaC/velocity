<script lang="ts">
  import { onMount, tick } from 'svelte';
  import CollapseDeletedRepoNotice from './CollapseDeletedRepoNotice.svelte';
  import Icon from './Icon.svelte';
  import {
    collapseCommands,
    newProjectCommands,
    onLayoutIntent,
    onProjectSelected,
    openProjectCommands,
    projectCommands,
    rosenMirrorCommands,
  } from './ipc';
  import type {
    CollapseRepository,
    CollapseRepositoryPickerView,
    DeletedCollapseRepository,
    DeletedRepositoryAction,
    LayoutAction,
    OpenProjectRequest,
    ProjectList,
    RepositoryPickerView,
  } from './ipc';
  import './theme/projects.css';

  let {
    name,
    project,
    collapse,
    collapseLiveState,
    rosen,
    send,
    select,
  }: {
    name: string;
    project: number | null;
    // The active project's Collapse mirror; null hides Open on Collapse (#429).
    collapse: string | null;
    // rosen.toml names the mirror's Collapse root, so Rosen sends Collapse its live
    // state; false offers Send Collapse its live state (#517).
    collapseLiveState: boolean;
    // The active project uses Rosen, so it offers Catch up from mirror (#501).
    rosen: boolean;
    send: (action: LayoutAction) => void;
    select: (identifier: number) => void;
  } = $props();

  let menu: HTMLElement;
  let projects = $state<ProjectList | null>(null);
  let error = $state('');
  let generation = 0;
  let active = false;
  // Open from GitHub / Collapse swaps the project list for a repository picker in the same menu.
  let showing = $state<'projects' | 'github' | 'collapse'>('projects');
  let query = $state('');
  let cloneTo = $state('');
  let defaultCloneTo = $state('');
  let picker = $state<RepositoryPickerView | null>(null);
  let collapsePicker = $state<CollapseRepositoryPickerView | null>(null);
  let enrollRepository = $state<string | null>(null);
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

  async function openOnCollapse(identifier: number) {
    menu.hidePopover();
    error = '';
    try {
      await collapseCommands.open(identifier);
    } catch (reason) {
      report(reason);
    }
  }

  // Rust catches the project up and reports the outcome as a toast; a
  // divergence also shows in the project's mirror notice.
  async function catchUpFromMirror(identifier: number) {
    menu.hidePopover();
    error = '';
    try {
      await rosenMirrorCommands.catchUp(identifier);
    } catch (reason) {
      report(reason);
    }
  }

  // Rust creates the Collapse repository, stores its push token with git and
  // lands the rosen.toml mirror change, then reports the outcome as a toast (#427, #610).
  let mirroring = $state(false);
  let mirrorDeleted = $state<DeletedCollapseRepository | null>(null);
  async function mirrorToCollapse(
    identifier: number,
    deletedAction: DeletedRepositoryAction | null = null,
  ) {
    if (mirroring) return;
    mirroring = true;
    error = '';
    try {
      const result = await collapseCommands.mirror(identifier, deletedAction);
      if (result.deleted_repository) {
        mirrorDeleted = result.deleted_repository;
      } else {
        mirrorDeleted = null;
        menu.hidePopover();
      }
    } catch (reason) {
      report(reason);
    } finally {
      mirroring = false;
    }
  }

  // Rust gives Rosen's state feed the mirror's push token and lands the rosen.toml
  // `collapse` root, then reports the outcome as a toast (#517).
  let linking = $state(false);
  async function sendLiveState(identifier: number) {
    if (linking) return;
    linking = true;
    error = '';
    try {
      await collapseCommands.sendLiveState(identifier);
      menu.hidePopover();
    } catch (reason) {
      report(reason);
    } finally {
      linking = false;
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

  async function searchCollapseRepositories(refresh: boolean) {
    const request = ++searchGeneration;
    reading = collapsePicker === null;
    try {
      const result = await openProjectCommands.collapseRepositories(
        query,
        refresh,
      );
      if (active && request === searchGeneration) {
        collapsePicker = result;
        error = '';
      }
    } catch (reason) {
      report(reason);
    } finally {
      if (active && request === searchGeneration) reading = false;
    }
  }

  function ensureDefaultCloneTo() {
    if (!defaultCloneTo) {
      void newProjectCommands
        .defaults()
        .then((defaults) => {
          if (active) defaultCloneTo = defaults.clone_to;
        })
        .catch(report);
    }
  }

  async function showGitHub() {
    showing = 'github';
    enrollRepository = null;
    query = '';
    error = '';
    await tick();
    search?.focus();
    ensureDefaultCloneTo();
    // Show the list read earlier at once, then read it again for new repositories.
    await searchRepositories(false);
    await searchRepositories(true);
  }

  async function showCollapse() {
    showing = 'collapse';
    enrollRepository = null;
    query = '';
    error = '';
    await tick();
    search?.focus();
    ensureDefaultCloneTo();
    await searchCollapseRepositories(false);
    await searchCollapseRepositories(true);
  }

  function openRepository(repository: string) {
    void open(
      { GitHub: { repository, clone_to: cloneTo } },
      `Opening ${repository}…`,
    );
  }

  function chooseCollapseRepository(repository: CollapseRepository) {
    if (repository.has_linked_server) {
      error = '';
      enrollRepository = repository.full_name;
      return;
    }
    openCollapseRepository(repository.full_name);
  }

  function openCollapseRepository(repository: string) {
    enrollRepository = null;
    void open(
      { Collapse: { repository, clone_to: cloneTo } },
      `Opening ${repository}…`,
    );
  }

  function openMachinesSettings() {
    enrollRepository = null;
    showing = 'projects';
    menu.hidePopover();
    send({ Request: { Navigation: 'Settings' } });
  }

  function openFirstMatch(event: KeyboardEvent) {
    if (event.key !== 'Enter') return;
    event.preventDefault();
    if (showing === 'collapse') {
      const firstRow = collapsePicker?.rows[0];
      if (firstRow) chooseCollapseRepository(firstRow);
      else if (collapsePicker?.typed)
        openCollapseRepository(collapsePicker.typed);
      return;
    }
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
        mirrorDeleted = null;
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
  title={name ? `Switch project � ${name}` : 'Switch project'}
  aria-haspopup="menu"
  popovertarget="project-menu"
  ><span class="project-name">{name || 'Open project'}</span><Icon
    name="chevron"
  /></button
>
<div
  bind:this={menu}
  id="project-menu"
  class="project-menu"
  class:github={showing === 'github' || showing === 'collapse'}
  popover="auto"
  role="menu"
  aria-label="Switch project"
  tabindex="-1"
  onkeydown={focusKey}
  ontoggle={(event) => {
    if (event.newState === 'open') void refresh(true).catch(report);
    else {
      showing = 'projects';
      enrollRepository = null;
      mirrorDeleted = null;
    }
  }}
>
  {#if showing === 'collapse'}
    <div class="menu-head">
      <button
        role="menuitem"
        onclick={async () => {
          if (enrollRepository) {
            enrollRepository = null;
            await tick();
            search?.focus();
            return;
          }
          showing = 'projects';
          await tick();
          menu.querySelector<HTMLElement>('button')?.focus();
        }}>‹ Back</button
      >{enrollRepository ? 'Enroll this machine' : 'Collapse repositories'}
    </div>
    {#if enrollRepository}
      <div class="enroll-offer">
        <p class="menu-note" role="status">
          <strong>{enrollRepository}</strong> already has a linked Rosen server. Enroll
          this machine with a join key from the project's owner (Settings → Workspaces
          → Machines) instead of starting a second server.
        </p>
        <button role="menuitem" onclick={openMachinesSettings}
          ><Icon name="settings" />Open Settings…</button
        >
        <button
          role="menuitem"
          onclick={async () => {
            enrollRepository = null;
            await tick();
            search?.focus();
          }}>Choose another repository</button
        >
      </div>
    {:else}
      <label class="inp"
        ><input
          bind:this={search}
          bind:value={query}
          placeholder="Search, or type owner/name"
          spellcheck="false"
          aria-label="Search your Collapse repositories"
          oninput={() => void searchCollapseRepositories(false)}
          onkeydown={openFirstMatch}
        /></label
      >
      <div class="repository-list">
        {#each collapsePicker?.rows ?? [] as repository (repository.full_name)}
          <button
            role="menuitem"
            title={repository.has_linked_server
              ? `${repository.full_name} already has a linked Rosen server — enroll this machine`
              : (repository.description ?? repository.full_name)}
            disabled={opening !== ''}
            onclick={() => chooseCollapseRepository(repository)}
            ><span class="repository-name">{repository.full_name}</span><span
              class="sub"
              >{repository.has_linked_server
                ? 'linked · enroll'
                : repository.archived
                  ? 'archived'
                  : repository.private
                    ? 'private'
                    : 'public'}</span
            ></button
          >
        {/each}
        {#if collapsePicker?.typed}
          <button
            role="menuitem"
            disabled={opening !== ''}
            onclick={() =>
              collapsePicker?.typed &&
              openCollapseRepository(collapsePicker.typed)}
            ><span class="repository-name">Open {collapsePicker.typed}</span
            ></button
          >
        {/if}
        {#if opening}<p class="menu-note" role="status">{opening}</p>
        {:else if reading}<p class="menu-note" role="status">
            Reading your Collapse repositories…
          </p>
        {:else if collapsePicker && collapsePicker.rows.length === 0 && !collapsePicker.typed}<p
            class="menu-note"
            role="status"
          >
            No repositories match.
          </p>
        {:else if collapsePicker && collapsePicker.hidden > 0}<p
            class="menu-note"
          >
            {collapsePicker.hidden} more · keep typing to narrow the list
          </p>{/if}
      </div>
      <div class="clone-row">
        <span>Clone to</span><label class="inp"
          ><input
            bind:value={cloneTo}
            placeholder={defaultCloneTo}
            spellcheck="false"
            aria-label="Clone to folder"
          /></label
        >
      </div>
    {/if}
  {:else if showing === 'github'}
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
          placeholder={defaultCloneTo}
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
    <button role="menuitem" onclick={() => void showCollapse()}
      ><Icon name="collapse" />Open from Collapse…</button
    >
    {#if collapse && project !== null}
      {@const identifier = project}
      <button
        role="menuitem"
        title={`collapse.sh/${collapse}`}
        onclick={() => void openOnCollapse(identifier)}
        >Open on Collapse ↗<span class="sub">{collapse}</span></button
      >
    {/if}
    {#if rosen && project !== null}
      {@const identifier = project}
      <button
        role="menuitem"
        title="Move Rosen's base forward to the mirror when another machine landed there"
        onclick={() => void catchUpFromMirror(identifier)}
        >Catch up from mirror</button
      >
      {#if !collapse}
        <button
          role="menuitem"
          disabled={mirroring}
          title="Create a Collapse repository for this project and push every land there"
          onclick={() => void mirrorToCollapse(identifier)}
          >Mirror to Collapse</button
        >
        {#if mirrorDeleted}
          <CollapseDeletedRepoNotice
            deleted={mirrorDeleted}
            compact
            busy={mirroring}
            onRestore={() => void mirrorToCollapse(identifier, 'restore')}
            onPurgeAndCreate={() =>
              void mirrorToCollapse(identifier, 'purge_and_create')}
          />
        {/if}
      {:else if !collapseLiveState}
        <button
          role="menuitem"
          disabled={linking}
          title="Collapse receives this project's commits only. Send its open changes, review queue and post-land state too, and let Collapse's Approve and Land act here"
          onclick={() => void sendLiveState(identifier)}
          >Send Collapse its live state</button
        >
      {/if}
    {/if}
    {#if mirroring}<p class="menu-note" role="status">
        Setting up the Collapse mirror…
      </p>{/if}
    {#if linking}<p class="menu-note" role="status">
        Linking Collapse to this project's live state…
      </p>{/if}
    {#if opening}<p class="menu-note" role="status">{opening}</p>{/if}
  {/if}
</div>
{#if error}<p class="layout-error" role="alert">{error}</p>{/if}
