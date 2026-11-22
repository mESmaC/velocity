<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { paneConnection } from './lib/pane-state.svelte';
  import WindowRecovery from './lib/WindowRecovery.svelte';
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
  import WorkspaceSettings from './lib/WorkspaceSettings.svelte';
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
    onCollapseLink,
    onLayoutDelta,
    onLayoutState,
    onLayoutIntent,
    onThreadTranscript,
    projectCommands,
    threadMenuCommands,
    aircRoutingCommands,
    reportWindowError,
    appearanceCommands,
  } from './lib/ipc';
  import {
    applyLayoutDelta,
    applyThreadTranscript,
    resetKeyedTranscript,
    type KeyedTranscript,
  } from './lib/transcript-state';
  import type {
    LayoutAction,
    LayoutDelta,
    LayoutSnapshot,
    LayoutIntent,
    ThreadMenuAction,
    ThreadMenuView,
    ThreadTranscriptChanged,
    MessageMode,
    UiZoomAdjustment,
    UiZoomView,
  } from './lib/ipc';
  import './lib/theme/layout.css';
  import './lib/theme/board.css';
  import './lib/theme/thread-view.css';
  import './lib/theme/duty.css';
  import './lib/theme/spec-interview.css';

  const LAYOUT_TIMEOUT_MS = 10_000;

  // Raw, not deep: Svelte would wrap every snapshot and transcript in fresh proxies (#321).
  // Deltas preserve unchanged slice references directly without deep comparison (#553).
  let layout = $state.raw<LayoutSnapshot | null>(null);
  let transcript = $state.raw<KeyedTranscript>(resetKeyedTranscript([], 0));
  $effect(() => {
    paneConnection.rosenReconnecting = layout?.rosen_reconnecting ?? false;
  });
  const revision = $derived(layout?.revision ?? 0);
  let error = $state('');
  let startupError = $state('');
  let latest: LayoutSnapshot | null = null;
  let latestTranscript: KeyedTranscript = resetKeyedTranscript([], 0);
  let resyncing = false;
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
  let requestedBrowser = $state('');
  let collapseNotice = $state('');
  let collapseNoticeEnrollment = $state(false);
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
  function scheduleFlush() {
    // Draw at most once per animation frame so high-frequency bursts never starve 60 fps.
    if (!frame)
      frame = window.requestAnimationFrame(() => {
        frame = 0;
        flush();
      });
  }

  function resyncLayout() {
    if (resyncing) return;
    resyncing = true;
    void layoutCommands
      .getState()
      .then((snapshot) => receive(snapshot))
      .catch(report)
      .finally(() => {
        resyncing = false;
      });
  }

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
    if (!latest || snapshot.revision >= latest.revision) {
      const sameThread =
        latest !== null &&
        latest.project_identifier === snapshot.project_identifier &&
        latest.active_thread?.identifier ===
          snapshot.active_thread?.identifier &&
        latestTranscript.revision ===
          (snapshot.active_thread?.transcript_revision ?? 0) &&
        latestTranscript.blocks.length ===
          (snapshot.active_thread?.blocks.length ?? 0);
      latest = snapshot;
      if (!sameThread) {
        latestTranscript = resetKeyedTranscript(
          snapshot.active_thread?.blocks ?? [],
          snapshot.active_thread?.transcript_revision ?? 0,
        );
      } else if (snapshot.active_thread) {
        latest = {
          ...snapshot,
          active_thread: {
            ...snapshot.active_thread,
            blocks: latestTranscript.blocks,
          },
        };
      }
    }
    scheduleFlush();
  }

  function receiveDelta(delta: LayoutDelta) {
    const outcome = applyLayoutDelta(latest, latestTranscript, delta);
    if (outcome.kind === 'ignored') return;
    if (outcome.kind === 'resync') {
      resyncLayout();
      return;
    }
    if (
      !latest ||
      outcome.snapshot.view !== latest.view ||
      outcome.snapshot.active_thread?.identifier !==
        latest.active_thread?.identifier
    ) {
      openPane = null;
    }
    latest = outcome.snapshot;
    latestTranscript = outcome.transcript;
    scheduleFlush();
  }

  function receiveTranscript(event: ThreadTranscriptChanged) {
    const outcome = applyThreadTranscript(latest, latestTranscript, event);
    if (outcome.kind === 'ignored') return;
    if (outcome.kind === 'resync') {
      resyncLayout();
      return;
    }
    latest = outcome.snapshot;
    latestTranscript = outcome.transcript;
    scheduleFlush();
  }

  function flush() {
    // Coalesce acknowledgements while typing so an older update cannot replace
    // text entered since that request. All decisions still come from Rust.
    if (pending) return;
    layout = latest;
    transcript = latestTranscript;
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

  function enqueue(
    operation: string,
    request: () => Promise<LayoutSnapshot | LayoutDelta>,
    clearsError = true,
  ) {
    pending += 1;
    queue = queue
      .then(async () => {
        let timeout = 0;
        try {
          const deadline = new Promise<never>((_, reject) => {
            timeout = window.setTimeout(() => {
              reject(
                new Error(
                  `${operation} timed out after ${LAYOUT_TIMEOUT_MS / 1000} seconds.`,
                ),
              );
            }, LAYOUT_TIMEOUT_MS);
          });
          // The IPC can still finish after the deadline. Rust publishes its own
          // revisioned updates, so a late reply does not need to hold this queue.
          const result = await Promise.race([request(), deadline]);
          if (clearsError) clearError();
          if ('base_revision' in result) receiveDelta(result);
          else receive(result);
        } finally {
          window.clearTimeout(timeout);
        }
      })
      .catch((reason: unknown) => {
        reportWindowError(operation, reason);
        report(reason);
      })
      .finally(() => {
        pending -= 1;
        flush();
      });
  }

  function send(action: LayoutAction) {
    if (!layout) return;
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
    const kind = typeof action === 'string' ? action : Object.keys(action)[0];
    enqueue(
      `Layout action ${kind}`,
      () => {
        window.console.debug(`Dispatching layout action ${kind}`);
        return layoutCommands.dispatch(action);
      },
      !keyReport,
    );
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
      enqueue('Delete swarm', () =>
        threadMenuCommands.deleteSwarm(project_identifier, thread_identifier),
      );
    else if (action === 'Delete')
      enqueue('Delete thread', () =>
        threadMenuCommands.delete(project_identifier, thread_identifier),
      );
    else
      threadMenuCommands
        .run(project_identifier, thread_identifier, action)
        .catch(report);
  }

  function undoDelete(deletion: number) {
    enqueue('Undo thread deletion', () =>
      threadMenuCommands.undoDelete(deletion),
    );
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
    enqueue('Switch project', () => projectCommands.switch(identifier));
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
      if (
        document.querySelector(
          '#compact-project-pulse:popover-open, #health-menu:popover-open',
        )
      )
        return;
      if (collapseNotice) {
        collapseNotice = '';
        collapseNoticeEnrollment = false;
        return;
      }
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
      const stopDelta = await onLayoutDelta((event) => {
        if (active) receiveDelta(event.payload.delta);
      });
      if (!active) {
        stopDelta();
        return;
      }
      stops.push(stopDelta);
      const stopTranscript = await onThreadTranscript((event) => {
        if (active) receiveTranscript(event.payload);
      });
      if (!active) {
        stopTranscript();
        return;
      }
      stops.push(stopTranscript);
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
      const stopCollapseLink = await onCollapseLink((event) => {
        if (!active) return;
        if (event.payload.kind === 'opened') {
          collapseNotice = '';
          collapseNoticeEnrollment = false;
          if (event.payload.open_draft) {
            intents.push({ Navigation: 'NewThread' });
            flush();
          }
          return;
        }
        collapseNotice = event.payload.notice;
        collapseNoticeEnrollment = event.payload.kind === 'needs_enrollment';
      });
      if (!active) {
        stopCollapseLink();
        return;
      }
      stops.push(stopCollapseLink);
      const snapshot = await layoutCommands.getState();
      if (active) receive(snapshot);
    }
    function startupFailed(reason: unknown) {
      if (!active || latest) return;
      const message = reason instanceof Error ? reason.message : String(reason);
      startupError = `Could not load the workspace. ${message}`;
      reportWindowError('startup', reason);
    }
    const startupDeadline = window.setTimeout(
      () => startupFailed('The layout did not respond in time.'),
      LAYOUT_TIMEOUT_MS,
    );
    void start()
      .catch(startupFailed)
      .finally(() => window.clearTimeout(startupDeadline));
    return () => {
      active = false;
      window.clearTimeout(startupDeadline);
      clearTimeout(errorTimer);
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
  {#if layout}
    <PaneBoundary name="title bar" {revision}
      ><TitleBar {layout} {send} {switchProject} /></PaneBoundary
    >
    <div
      class="workspace"
      class:board-mode={layout.view === 'Board'}
      data-open-pane={openPane}
    >
      <!-- The Board replaces the sidebar, one thread and the rail with every thread at once. -->
      {#if layout.view === 'Board'}<PaneBoundary name="board" {revision}
          ><Board
            {layout}
            {send}
            openBrowser={(thread) => {
              requestedBrowser = `${layout?.project_identifier}:${thread}`;
              send({ OpenThread: thread });
            }}
          /></PaneBoundary
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
                blockIds={transcript.ids}
                interview={layout.active_thread.interview}
                {send}
              />{:else if layout.view === 'Thread' && layout.active_thread}<ThreadView
                {layout}
                thread={layout.active_thread}
                blockIds={transcript.ids}
                browserRequested={requestedBrowser ===
                  `${layout.project_identifier}:${layout.active_thread.identifier}`}
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
              />{:else if layout.view === 'Settings' && layout.settings_tab === 'workspaces'}
              {#key layout.project_identifier}
                <WorkspaceSettings
                  mirror={layout.rosen_mirror}
                  projectIdentifier={layout.project_identifier}
                  {send}
                />
              {/key}
            {:else if layout.view === 'Settings' && layout.settings_tab === 'providers'}<ProvidersSettings
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
    </div>{:else}<WindowRecovery error={startupError} />{/if}
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
{#if layout?.duty_dialog_open}<PaneBoundary name="review dialog" {revision}
    ><DutyDialog
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
        collapse={layout.collapse_repository !== null}
        {send}
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
{#if collapseNotice}
  <div
    class="collapse-link-notice"
    role="status"
    aria-label="Collapse link notice"
  >
    <span>{collapseNotice}</span>
    {#if collapseNoticeEnrollment}
      <button
        type="button"
        class="mini"
        onclick={() => {
          collapseNotice = '';
          collapseNoticeEnrollment = false;
          send({ Request: { Navigation: 'Settings' } });
        }}>Open Settings</button
      >
    {/if}
    <button
      type="button"
      class="mini"
      aria-label="Dismiss notice"
      onclick={() => {
        collapseNotice = '';
        collapseNoticeEnrollment = false;
      }}>Dismiss</button
    >
  </div>
{/if}
<Tooltip />
{#if error || layout?.launch_notice}<p class="layout-error" role="alert">
    {error || layout?.launch_notice}
  </p>{/if}
