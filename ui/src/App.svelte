<script lang="ts">
  import { onMount, tick } from 'svelte';
  import TitleBar from './lib/TitleBar.svelte';
  import Sidebar from './lib/Sidebar.svelte';
  import Rail from './lib/Rail.svelte';
  import Home from './lib/Home.svelte';
  import ThreadView from './lib/ThreadView.svelte';
  import DutyView from './lib/DutyView.svelte';
  import SwarmView from './lib/SwarmView.svelte';
  import SwarmDialog from './lib/SwarmDialog.svelte';
  import AircModal from './lib/AircModal.svelte';
  import SpecInterview from './lib/SpecInterview.svelte';
  import AgentsEditor from './lib/AgentsEditor.svelte';
  import CommandPalette from './lib/CommandPalette.svelte';
  import WorktreeSettings from './lib/WorktreeSettings.svelte';
  import NewProjectDialog from './lib/NewProjectDialog.svelte';
  import DutyDialog from './lib/DutyDialog.svelte';
  import ThreadMenu from './lib/ThreadMenu.svelte';
  import Toast from './lib/Toast.svelte';
  import {
    layoutCommands,
    onLayoutState,
    onLayoutIntent,
    projectCommands,
    threadMenuCommands,
  } from './lib/ipc';
  import type {
    LayoutAction,
    LayoutSnapshot,
    LayoutIntent,
    ThreadMenuAction,
    ThreadMenuView,
  } from './lib/ipc';
  import './lib/theme/layout.css';
  import './lib/theme/thread-view.css';
  import './lib/theme/duty.css';
  import './lib/theme/spec-interview.css';

  let layout = $state<LayoutSnapshot | null>(null);
  let error = $state('');
  let latest: LayoutSnapshot | null = null;
  let pending = 0;
  let newProjectSubmits = $state(0);
  let dutySubmits = $state(0);
  let swarmSubmits = $state(0);
  let agentsSaves = $state(0);
  let queue = Promise.resolve();
  const intents: LayoutIntent[] = [];
  const shortcutKeys = new Set([
    'k',
    'n',
    ',',
    'alt',
    '/',
    't',
    'w',
    'g',
    'a',
    'escape',
    'enter',
    'i',
    'r',
    'f',
    'v',
    's',
    'l',
    'd',
    'e',
    'arrowdown',
    'arrowup',
  ]);

  function report(reason: unknown) {
    error = reason instanceof Error ? reason.message : String(reason);
  }

  function receive(snapshot: LayoutSnapshot) {
    if (!latest || snapshot.revision >= latest.revision) latest = snapshot;
    flush();
  }

  function flush() {
    // Coalesce acknowledgements while typing so an older snapshot cannot replace
    // text entered since that request. All decisions still come from Rust.
    if (pending) return;
    layout = latest;
    const requested = intents.splice(0);
    void tick().then(() => requested.forEach(focusIntent));
  }

  function focusIntent(intent: LayoutIntent) {
    if (typeof intent === 'string' || !('Navigation' in intent)) return;
    const navigation = intent.Navigation;
    if (navigation === 'SaveAgents' && layout?.view === 'Agents') {
      agentsSaves += 1;
      return;
    }
    if (navigation === 'Submit' && layout?.new_project_open) {
      newProjectSubmits += 1;
      return;
    }
    if (navigation === 'Submit' && layout?.duty_dialog_open) {
      dutySubmits += 1;
      return;
    }
    if (navigation === 'Submit' && layout?.swarm_dialog) {
      swarmSubmits += 1;
      return;
    }
    const target =
      navigation === 'Search'
        ? 'thread-search'
        : navigation === 'Task'
          ? 'task-input'
          : navigation === 'NewThread'
            ? 'home-composer'
            : null;
    if (target) document.getElementById(target)?.focus();
  }

  function enqueue(request: () => Promise<LayoutSnapshot>) {
    pending += 1;
    queue = queue
      .then(async () => receive(await request()))
      .catch(report)
      .finally(() => {
        pending -= 1;
        flush();
      });
  }

  function send(action: LayoutAction) {
    enqueue(() => layoutCommands.dispatch(action));
  }

  let threadMenu = $state<{
    view: ThreadMenuView;
    x: number;
    y: number;
  } | null>(null);

  // Right-click a sidebar thread (F12). Rust decides the entries; this places them.
  function openThreadMenu(identifier: string, x: number, y: number) {
    threadMenu = null;
    threadMenuCommands
      .get(Number(identifier))
      .then((view) => (threadMenu = view ? { view, x, y } : null))
      .catch(report);
  }

  function chooseThreadAction(action: ThreadMenuAction) {
    const menu = threadMenu?.view;
    threadMenu = null;
    if (!menu) return;
    const { project_identifier, thread_identifier } = menu;
    if (action === 'Delete')
      enqueue(() =>
        threadMenuCommands.delete(project_identifier, thread_identifier),
      );
    else
      threadMenuCommands
        .run(project_identifier, thread_identifier, action)
        .catch(report);
  }

  function undoDelete(deletion: number) {
    enqueue(() => threadMenuCommands.undoDelete(deletion));
  }

  function switchProject(identifier: number) {
    enqueue(() => projectCommands.switch(identifier));
  }

  function keydown(event: KeyboardEvent) {
    if (!layout || event.isComposing || event.key === 'Process') return;
    if (event.key === 'Alt' && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      if (!event.repeat) send({ NumberHints: 'AltDown' });
      return;
    }
    const typing =
      event.target instanceof Element &&
      !!event.target.closest(
        'input, textarea, select, [contenteditable]:not([contenteditable="false"]), [role="textbox"]',
      );
    const key = event.key.toLowerCase();
    const recognized = shortcutKeys.has(key) || /^Digit[0-9]$/.test(event.code);
    if (!recognized) return;
    // Cancel webview defaults synchronously; Rust decides whether to dispatch.
    if (
      (event.ctrlKey && ['k', 'n', ',', 's', 'enter'].includes(key)) ||
      (event.altKey && /^Digit[1-9]$/.test(event.code)) ||
      (!typing &&
        !event.ctrlKey &&
        !event.altKey &&
        !event.metaKey &&
        key === '/') ||
      (layout.palette_open && ['arrowdown', 'arrowup', 'enter'].includes(key))
    )
      event.preventDefault();
    send({
      Keyboard: {
        key: event.key,
        code: event.code,
        control: event.ctrlKey,
        shift: event.shiftKey,
        alt: event.altKey,
        meta_held: event.metaKey,
        typing,
      },
    });
  }

  onMount(() => {
    let active = true;
    const stops: (() => void)[] = [];
    async function start() {
      const stopState = await onLayoutState((event) => {
        if (active) receive(event.payload.state);
      });
      if (!active) {
        stopState();
        return;
      }
      stops.push(stopState);
      const stopIntent = await onLayoutIntent((event) => {
        if (active) {
          intents.push(event.payload.intent);
          flush();
        }
      });
      if (!active) {
        stopIntent();
        return;
      }
      stops.push(stopIntent);
      const snapshot = await layoutCommands.getState();
      if (active) receive(snapshot);
    }
    void start().catch(report);
    return () => {
      active = false;
      stops.forEach((stop) => stop());
    };
  });
</script>

<svelte:window
  onkeydown={keydown}
  onkeyup={(event) => {
    if (event.key === 'Alt') send({ NumberHints: 'AltUp' });
  }}
  onblur={() => send({ NumberHints: 'Blur' })}
/>

<div class="app-shell">
  <TitleBar {layout} {send} {switchProject} />
  {#if layout}<div class="workspace">
      <Sidebar {layout} {send} openMenu={openThreadMenu} />
      <main class="main-view" aria-label="Velocity workspace">
        {#if layout.view === 'Agents' && layout.project_identifier !== null}
          {#key layout.project_identifier}<AgentsEditor
              project={layout.project_identifier}
              saveRequests={agentsSaves}
            />{/key}
        {:else if layout.view === 'Home'}<Home
            {layout}
            {send}
          />{:else if layout.active_thread?.interview}<SpecInterview
            {layout}
            thread={layout.active_thread}
            interview={layout.active_thread.interview}
            {send}
          />{:else if layout.view === 'Thread' && layout.active_thread}<ThreadView
            {layout}
            thread={layout.active_thread}
            {send}
          />{:else if layout.view === 'Duty' && layout.active_duty}<DutyView
            {layout}
            duty={layout.active_duty}
            {send}
            onResult={receive}
            onError={report}
          />{:else if layout.view === 'Swarm' && layout.active_swarm}<SwarmView
            {layout}
            swarm={layout.active_swarm}
            {send}
          />{:else if layout.view === 'Settings' && layout.settings_tab === 'worktrees'}<WorktreeSettings
            {send}
          />{/if}
      </main>
      <Rail {layout} {send} />
    </div>{:else}<p class="empty" role="status">Opening workspace…</p>{/if}
</div>
{#if layout?.palette_open}<CommandPalette {layout} {send} />{/if}
{#if layout?.new_project_open}<NewProjectDialog
    {send}
    submitRequests={newProjectSubmits}
  />{/if}
{#if layout?.duty_dialog_open}<DutyDialog
    initialRole={layout.duty_dialog_role}
    {send}
    submitRequests={dutySubmits}
    onResult={receive}
  />{/if}
{#if layout?.swarm_dialog}{#key `${layout.swarm_dialog.source}-${layout.swarm_dialog.into}`}<SwarmDialog
      request={layout.swarm_dialog}
      {send}
      submitRequests={swarmSubmits}
      onResult={receive}
    />{/key}{/if}
{#if layout?.airc_open && layout.project_identifier !== null}{#key layout.project_identifier}<AircModal
      project={layout.project_identifier}
      {send}
      onResult={receive}
    />{/key}{/if}
{#if threadMenu}<ThreadMenu
    menu={threadMenu.view}
    x={threadMenu.x}
    y={threadMenu.y}
    choose={chooseThreadAction}
    close={() => (threadMenu = null)}
  />{/if}
<Toast toast={layout?.toast ?? null} undo={undoDelete} />
{#if error || layout?.launch_notice}<p class="layout-error" role="alert">
    {error || layout?.launch_notice}
  </p>{/if}
