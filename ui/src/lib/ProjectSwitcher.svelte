<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Icon from './Icon.svelte';
  import { onLayoutIntent, onProjectSelected, projectCommands } from './ipc';
  import type { LayoutAction, ProjectList } from './ipc';
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
  popover="auto"
  role="menu"
  aria-label="Switch project"
  tabindex="-1"
  onkeydown={focusKey}
  ontoggle={(event) => {
    if (event.newState === 'open') void refresh(true).catch(report);
  }}
>
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
</div>
{#if error}<p class="layout-error" role="alert">{error}</p>{/if}
