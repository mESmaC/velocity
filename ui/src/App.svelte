<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { paneConnection } from './lib/pane-state.svelte';
  import PaneState from './lib/PaneState.svelte';
  import TitleBar from './lib/TitleBar.svelte';
  import Board from './lib/Board.svelte';
  import Sidebar from './lib/Sidebar.svelte';
  import Rail from './lib/Rail.svelte';
  import Home from './lib/Home.svelte';
  import ThreadView from './lib/ThreadView.svelte';
  import DutyView from './lib/DutyView.svelte';
  import SwarmView from './lib/SwarmView.svelte';
  import GoalSwarmView from './lib/GoalSwarmView.svelte';
  import SwarmDialog from './lib/SwarmDialog.svelte';
  import GoalSwarmDialog from './lib/GoalSwarmDialog.svelte';
  import AircModal from './lib/AircModal.svelte';
  import SpecInterview from './lib/SpecInterview.svelte';
  import AgentsEditor from './lib/AgentsEditor.svelte';
  import CommandPalette from './lib/CommandPalette.svelte';
  import WorktreeSettings from './lib/WorktreeSettings.svelte';
  import ProvidersSettings from './lib/ProvidersSettings.svelte';
  import AgentSettings from './lib/AgentSettings.svelte';
  import ProofsSettings from './lib/ProofsSettings.svelte';
  import SkillsSettings from './lib/SkillsSettings.svelte';
  import AppearanceSettings from './lib/AppearanceSettings.svelte';
  import NewProjectDialog from './lib/NewProjectDialog.svelte';
  import DutyDialog from './lib/DutyDialog.svelte';
  import ThreadMenu from './lib/ThreadMenu.svelte';
  import Toast from './lib/Toast.svelte';
  import Tooltip from './lib/Tooltip.svelte';
  import PaneBoundary from './lib/PaneBoundary.svelte';
  import {
    layoutCommands,
    onLayoutState,
    onLayoutIntent,
    projectCommands,
    threadMenuCommands,
    aircRoutingCommands,
    reportWindowError,
    appearanceCommands,
  } from './lib/ipc';
  import { reuseUnchanged } from './lib/reuse-unchanged';
  import type {
    LayoutAction,
    LayoutSnapshot,
    LayoutIntent,
    ThreadMenuAction,
    ThreadMenuView,
    MessageMode,
    UiZoomAdjustment,
    UiZoomView,
  } from './lib/ipc';
  import './lib/theme/layout.css';
  import './lib/theme/board.css';
  import './lib/theme/thread-view.css';
  import './lib/theme/duty.css';
  import './lib/theme/spec-interview.css';

  // Raw, not deep: Svelte would wrap every new snapshot, transcripts included, in fresh
  // proxies (#321). Snapshots are replaced whole and never changed in place.
  let layout = $state.raw<LayoutSnapshot | null>(null);
  $effect(() => {
    paneConnection.rosenReconnecting = layout?.rosen_reconnecting ?? false;
  });
  const revision = $derived(layout?.revision ?? 0);
  let error = $state('');
  let startupError = $state('');
  let latest: LayoutSnapshot | null = null;
  let pending = 0;
  let newProjectSubmits = $state(0);
  let dutySubmits = $state(0);
  let swarmSubmits = $state(0);
  let goalSubmits = $state(0);
  let agentsSaves = $state(0);
  let queue = Promise.resolve();
  let zoom = $state.raw<UiZoomView | null>(null);
  let zoomError = $state('');
  let zoomQueue = Promise.resolve();
  let openPane = $state<'threads' | 'activity' | null>(null);
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
    'b',
    'arrowdown',
    'arrowup',
  ]);

  let errorTimer: ReturnType<typeof setTimeout> | undefined;
  function clearError() {
    clearTimeout(errorTimer);
    error = '';
  }
  function report(reason: unknown) {
    clearError();
    error = reason instanceof Error ? reason.message : String(reason);
    errorTimer = setTimeout(clearError, 4000);
  }

  $effect(() => {
    if (zoom)
      document.documentElement.style.setProperty(
        '--ui-zoom',
        `${zoom.percent}%`,
      );
  });

  async function readZoom() {
    zoomError = '';
    try {
      zoom = await appearanceCommands.readZoom();
    } catch (reason) {
      zoomError = String(reason);
    }
  }

  function adjustZoom(adjustment: UiZoomAdjustment) {
    zoomQueue = zoomQueue
      .then(async () => {
        zoom = await appearanceCommands.adjustZoom(adjustment);
      })
      .catch(report);
  }

  let frame = 0;
  function receive(snapshot: LayoutSnapshot) {
    if (
      !latest ||
      (snapshot.revision >= latest.revision &&
        (snapshot.view !== latest.view ||
          snapshot.active_thread?.identifier !==
            latest.active_thread?.identifier))
    ) {
      openPane = null;
    }
    if (!latest || snapshot.revision >= latest.revision) latest = snapshot;
    // A swarm publishes many snapshots a second (#273). Draw only the newest, once per
    // frame, rather than re-rendering the whole window for each one.
    if (!frame)
      frame = window.requestAnimationFrame(() => {
        frame = 0;
        flush();
      });
  }

  function flush() {
    // Coalesce acknowledgements while typing so an older snapshot cannot replace
    // text entered since that request. All decisions still come from Rust.
    if (pending) return;
    layout = reuseUnchanged(layout, latest);
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
    if (navigation === 'Submit' && layout?.goal_dialog) {
      goalSubmits += 1;
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
    if (target) {
      openPane =
        navigation === 'Search'
          ? 'threads'
          : navigation === 'Task'
            ? 'activity'
            : null;
      void tick().then(() => document.getElementById(target)?.focus());
    }
  }

  function enqueue(request: () => Promise<LayoutSnapshot>, clearsError = true) {
    pending += 1;
    queue = queue
      .then(async () => {
        const snapshot = await request();
        if (clearsError) clearError();
        receive(snapshot);
      })
      .catch(report)
      .finally(() => {
        pending -= 1;
        flush();
      });
  }

  function send(action: LayoutAction) {
    if (
      typeof action === 'object' &&
      ('OpenThread' in action || 'Request' in action)
    ) {
      openPane = null;
    }
    // Key reports go out with every keypress. Enter's own report follows the send it
    // started, so letting it clear errors hid a failed send's reason at once (#274).
    const keyReport =
      typeof action === 'object' &&
      ('Keyboard' in action || 'NumberHints' in action);
    enqueue(() => layoutCommands.dispatch(action), !keyReport);
  }

  let threadMenu = $state<{
    view: ThreadMenuView;
    x: number;
    y: number;
    opener: HTMLElement;
  } | null>(null);
  let threadMenuRequest = 0;

  function closeThreadMenu() {
    threadMenuRequest += 1;
    threadMenu = null;
  }

  // Right-click a sidebar thread (F12). Rust decides the entries; this places them.
  function openThreadMenu(
    identifier: string,
    x: number,
    y: number,
    opener: HTMLElement,
  ) {
    closeThreadMenu();
    const request = threadMenuRequest;
    threadMenuCommands
      .get(Number(identifier))
      .then((view) => {
        if (request === threadMenuRequest && view && opener.isConnected)
          threadMenu = { view, x, y, opener };
      })
      .catch(report);
  }

  function chooseThreadAction(action: ThreadMenuAction) {
    const menu = threadMenu?.view;
    closeThreadMenu();
    if (!menu) return;
    const { project_identifier, thread_identifier } = menu;
    if (action === 'Delete' && menu.swarm)
      enqueue(() =>
        threadMenuCommands.deleteSwarm(project_identifier, thread_identifier),
      );
    else if (action === 'Delete')
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

  function chooseThreadMode(mode: MessageMode) {
    const menu = threadMenu?.view;
    closeThreadMenu();
    if (!menu?.message_mode) return;
    aircRoutingCommands
      .setMode(menu.project_identifier, menu.message_mode.nick, mode)
      .catch(report);
  }

  function switchProject(identifier: number) {
    openPane = null;
    enqueue(() => projectCommands.switch(identifier));
  }

  async function togglePane(pane: 'threads' | 'activity') {
    openPane = openPane === pane ? null : pane;
    if (openPane) {
      await tick();
      document.getElementById(`workspace-${pane}`)?.focus();
    }
  }

  function zoomShortcut(event: KeyboardEvent): boolean {
    if (!event.ctrlKey || event.altKey || event.metaKey) return false;
    const adjustment =
      event.key === '=' || event.key === '+'
        ? 'Increase'
        : event.key === '-'
          ? 'Decrease'
          : event.key === '0'
            ? 'Reset'
            : null;
    if (!adjustment) return false;
    event.preventDefault();
    adjustZoom(adjustment);
    return true;
  }

  function dismissCompactPane(snapshot: LayoutSnapshot) {
    if (
      openPane &&
      !snapshot.palette_open &&
      !snapshot.airc_open &&
      !snapshot.new_project_open &&
      !snapshot.duty_dialog_open &&
      !snapshot.swarm_dialog &&
      !snapshot.goal_dialog &&
      !threadMenu
    ) {
      const button = document.getElementById(`pane-switch-${openPane}`);
      if (button?.getClientRects().length) {
        openPane = null;
        button.focus();
        return true;
      }
    }
    return false;
  }

  function keydown(event: KeyboardEvent) {
    if (
      event.isComposing ||
      event.key === 'Process' ||
      zoomShortcut(event) ||
      !layout
    )
      return;
    if (event.key === 'Escape') {
      if (event.defaultPrevented || document.querySelector('dialog:modal'))
        return;
      if (document.querySelector('#compact-project-pulse:popover-open')) return;
      if (dismissCompactPane(layout)) return;
      closeThreadMenu();
    }
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
    paneConnection.online = window.navigator.onLine;
    let active = true;
    const stops: (() => void)[] = [];
    zoomQueue = readZoom();
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
    void start().catch((reason: unknown) => {
      startupError = String(reason);
      reportWindowError('startup', reason);
      report(reason);
    });
    return () => {
      active = false;
      window.cancelAnimationFrame(frame);
      stops.forEach((stop) => stop());
    };
  });
</script>

<svelte:window
  ononline={() => (paneConnection.online = true)}
  onoffline={() => (paneConnection.online = false)}
  onkeydown={keydown}
  onkeyup={(event) => {
    if (event.key === 'Alt') send({ NumberHints: 'AltUp' });
  }}
  onblur={() => send({ NumberHints: 'Blur' })}
/>

<!-- Each pane fails on its own and redraws from the next snapshot (#273). -->
<div class="app-shell">
  <PaneBoundary name="title bar" {revision}
    ><TitleBar {layout} {send} {switchProject} /></PaneBoundary
  >
  {#if layout}<div
      class="workspace"
      class:board-mode={layout.view === 'Board'}
      data-open-pane={openPane}
    >
      <!-- The Board replaces the sidebar, one thread and the rail with every thread at once. -->
      {#if layout.view === 'Board'}<PaneBoundary name="board" {revision}
          ><Board {layout} {send} /></PaneBoundary
        >{:else}
        <div class="pane-switch" role="group" aria-label="Workspace panes">
          <button
            id="pane-switch-threads"
            class="pane-switch-threads btn"
            aria-controls="workspace-threads"
            aria-expanded={openPane === 'threads'}
            onclick={() => void togglePane('threads')}>Threads</button
          >
          <button
            id="pane-switch-activity"
            class="pane-switch-activity btn"
            aria-controls="workspace-activity"
            aria-expanded={openPane === 'activity'}
            onclick={() => void togglePane('activity')}>Activity</button
          >
        </div>
        <div class="workspace-threads" id="workspace-threads" tabindex="-1">
          <PaneBoundary name="sidebar" {revision}
            ><Sidebar {layout} {send} openMenu={openThreadMenu} /></PaneBoundary
          >
        </div>
        <main class="main-view" aria-label="Velocity workspace">
          <PaneBoundary name="main view" {revision}>
            {#if layout.view === 'Agents' && layout.project_identifier !== null}
              {#key layout.project_identifier}<AgentsEditor
                  project={layout.project_identifier}
                  saveRequests={agentsSaves}
                />{/key}
            {:else if layout.view === 'Home'}<Home
                {layout}
                {send}
              />{:else if layout.view.startsWith('Spec') && layout.active_thread?.interview}<SpecInterview
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
              />{:else if layout.view === 'Swarm' && layout.active_goal_swarm}<GoalSwarmView
                {layout}
                swarm={layout.active_goal_swarm}
                {send}
              />{:else if layout.view === 'Settings' && layout.settings_tab === 'worktrees'}<WorktreeSettings
                mirror={layout.rosen_mirror}
                projectIdentifier={layout.project_identifier}
                {send}
              />{:else if layout.view === 'Settings' && layout.settings_tab === 'providers'}<ProvidersSettings
                {send}
              />{:else if layout.view === 'Settings' && layout.settings_tab === 'appearance'}<AppearanceSettings
                {zoom}
                error={zoomError}
                retry={readZoom}
                adjust={adjustZoom}
                {send}
              />{:else if layout.view === 'Settings' && layout.settings_tab === 'skills'}<SkillsSettings
                {send}
                projectIdentifier={layout.project_identifier}
              />{:else if layout.view === 'Settings' && layout.settings_tab === 'agent-settings'}<AgentSettings
                {send}
              />{:else if layout.view === 'Settings' && layout.settings_tab === 'proofs' && layout.project_identifier !== null}{#key layout.project_identifier}<ProofsSettings
                  project={layout.project_identifier}
                  {send}
                />{/key}{/if}
          </PaneBoundary>
        </main>
        <div class="workspace-activity" id="workspace-activity" tabindex="-1">
          <PaneBoundary name="rail" {revision}
            ><Rail {layout} {send} /></PaneBoundary
          >
        </div>{/if}
    </div>{:else}<PaneState
      name="Threads"
      loading
      error={startupError}
      retry={async () => {
        try {
          receive(await layoutCommands.getState());
          startupError = '';
          clearError();
        } catch (reason) {
          startupError = String(reason);
          report(reason);
        }
      }}
    />{/if}
</div>
{#if layout?.palette_open}<PaneBoundary name="command palette" {revision}
    ><CommandPalette {layout} {send} /></PaneBoundary
  >{/if}
{#if layout?.new_project_open}<PaneBoundary name="new project" {revision}
    ><NewProjectDialog
      {send}
      submitRequests={newProjectSubmits}
    /></PaneBoundary
  >{/if}
{#if layout?.duty_dialog_open}<PaneBoundary name="duty dialog" {revision}
    ><DutyDialog
      initialRole={layout.duty_dialog_role}
      {send}
      submitRequests={dutySubmits}
      onResult={receive}
    /></PaneBoundary
  >{/if}
{#if layout?.swarm_dialog}{#key `${layout.swarm_dialog.source}-${layout.swarm_dialog.into}`}<PaneBoundary
      name="swarm dialog"
      {revision}
      ><SwarmDialog
        request={layout.swarm_dialog}
        {send}
        submitRequests={swarmSubmits}
        onResult={receive}
      /></PaneBoundary
    >{/key}{/if}
{#if layout?.goal_dialog}{#key layout.goal_dialog.into}<PaneBoundary
      name="goal swarm dialog"
      {revision}
      ><GoalSwarmDialog
        request={layout.goal_dialog}
        {send}
        submitRequests={goalSubmits}
        onResult={receive}
      /></PaneBoundary
    >{/key}{/if}
{#if layout?.airc_open && layout.project_identifier !== null}{#key layout.project_identifier}<PaneBoundary
      name="AIRC"
      {revision}
      ><AircModal
        project={layout.project_identifier}
        {send}
        onResult={receive}
      /></PaneBoundary
    >{/key}{/if}
{#if threadMenu}<ThreadMenu
    menu={threadMenu.view}
    x={threadMenu.x}
    y={threadMenu.y}
    opener={threadMenu.opener}
    choose={chooseThreadAction}
    chooseMode={chooseThreadMode}
    close={closeThreadMenu}
  />{/if}
<Toast toast={layout?.toast ?? null} undo={undoDelete} />
<Tooltip />
{#if error || layout?.launch_notice}<p class="layout-error" role="alert">
    {error || layout?.launch_notice}
  </p>{/if}
