<script lang="ts">
  import { onMount, tick } from 'svelte';
  import {
    browserCommands,
    onBrowserFrame,
    type BrowserFrame,
    type BrowserInput,
    type BrowserView,
  } from './ipc';

  let { project, thread }: { project: number; thread: number } = $props();
  let view = $state.raw<BrowserView | null>(null);
  let frame = $state.raw<BrowserFrame['frame']>(null);
  let canvas = $state<globalThis.HTMLCanvasElement | null>(null);
  let busy = $state(false);
  let error = $state('');
  let sequence = 0;
  let inputs = Promise.resolve();
  const selected = $derived(view?.tabs.find((tab) => tab.id === view?.active));

  onMount(() => {
    let closed = false;
    let release: (() => void) | undefined;
    let viewId: string | undefined;
    let pendingFrame: BrowserFrame | undefined;
    void (async () => {
      try {
        release = await onBrowserFrame(({ payload }) => {
          if (
            payload.project !== project ||
            payload.thread !== thread ||
            closed
          )
            return;
          if (viewId === undefined) {
            pendingFrame = payload;
          } else if (payload.view.view_id === viewId) {
            update(payload);
          }
        });
        if (closed) {
          release();
          return;
        }
        const opened = await browserCommands.open(project, thread);
        viewId = opened.view_id;
        if (closed) {
          await browserCommands.close(project, thread, viewId);
          return;
        }
        view = opened;
        if (pendingFrame?.view.view_id === opened.view_id) update(pendingFrame);
        pendingFrame = undefined;
      } catch (failure) {
        if (!closed) error = String(failure);
      }
    })();
    return () => {
      closed = true;
      view = null;
      sequence += 1;
      release?.();
      if (viewId)
        void browserCommands.close(project, thread, viewId).catch(() => {});
    };
  });

  function update(payload: BrowserFrame) {
    if (view?.active !== payload.view.active) {
      frame = null;
      sequence += 1;
    }
    view = payload.view;
    if (payload.frame) {
      frame = payload.frame;
      void draw(payload.frame, ++sequence);
    }
  }

  async function draw(
    image: NonNullable<BrowserFrame['frame']>,
    current: number,
  ) {
    const bytes = Uint8Array.from(window.atob(image.jpeg), (value) =>
      value.charCodeAt(0),
    );
    const bitmap = await window.createImageBitmap(
      new globalThis.Blob([bytes], { type: 'image/jpeg' }),
    );
    await tick();
    if (canvas && current === sequence) {
      canvas.width = bitmap.width;
      canvas.height = bitmap.height;
      canvas.getContext('2d')?.drawImage(bitmap, 0, 0);
    }
    bitmap.close();
  }

  async function transfer() {
    if (!view || busy) return;
    const { view_id, owner } = view;
    busy = true;
    error = '';
    try {
      if (owner) {
        await inputs;
        await browserCommands.handBack(project, thread, view_id);
      } else {
        await browserCommands.takeOver(project, thread, view_id);
      }
      if (view?.view_id === view_id) view = { ...view, owner: !owner };
      if (!owner) {
        await tick();
        canvas?.focus({ preventScroll: true });
      }
    } catch (failure) {
      error = String(failure);
    } finally {
      busy = false;
    }
  }

  function switchTab(tab: string) {
    if (!view?.owner) return;
    void browserCommands
      .switchTab(project, thread, view.view_id, tab)
      .catch((failure) => {
        error = String(failure);
      });
  }

  function send(input: BrowserInput) {
    if (!view?.owner || !view.active || busy) return;
    const { active, view_id } = view;
    inputs = inputs
      .then(async () => {
        if (view?.view_id !== view_id || !view.owner) return;
        await browserCommands.input(project, thread, view_id, active, input);
      })
      .catch((failure) => {
        error = String(failure);
      });
  }

  function point(event: MouseEvent) {
    if (!frame || !canvas || frame.width === null || frame.height === null)
      return null;
    const bounds = canvas.getBoundingClientRect();
    if (!bounds.width || !bounds.height) return null;
    return {
      x: ((event.clientX - bounds.left) / bounds.width) * frame.width,
      y: ((event.clientY - bounds.top) / bounds.height) * frame.height,
    };
  }

  function click(event: MouseEvent) {
    if (!view?.owner) return;
    const coordinates = point(event);
    if (!coordinates) return;
    canvas?.focus({ preventScroll: true });
    send({ kind: 'click', ...coordinates });
  }

  function scroll(event: globalThis.WheelEvent) {
    if (!view?.owner) return;
    const coordinates = point(event);
    if (!coordinates) return;
    event.preventDefault();
    send({
      kind: 'scroll',
      ...coordinates,
      horizontal: event.deltaX,
      vertical: event.deltaY,
    });
  }

  function keydown(event: KeyboardEvent) {
    if (!view?.owner) return;
    event.stopPropagation();
    if (event.key === 'Escape' && event.shiftKey) {
      event.preventDefault();
      void transfer();
      return;
    }
    if (
      ['Shift', 'Control', 'Alt', 'Meta', 'Dead', 'Unidentified'].includes(
        event.key,
      )
    )
      return;
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'v')
      return;
    event.preventDefault();
    const modifiers =
      Number(event.altKey) +
      2 * Number(event.ctrlKey) +
      4 * Number(event.metaKey) +
      8 * Number(event.shiftKey);
    send({ kind: 'key', key: event.key, modifiers });
  }

  function paste(event: globalThis.ClipboardEvent) {
    if (!view?.owner) return;
    const text = event.clipboardData?.getData('text/plain');
    if (text) {
      event.preventDefault();
      send({ kind: 'text', text });
    }
  }
</script>

<section class="thread-browser" aria-label="Agent browser">
  {#if view?.tabs.length}
    <div class="browser-tabs" role="group" aria-label="Browser tabs">
      {#each view.tabs as tab (tab.id)}
        <button
          class="btn"
          type="button"
          aria-pressed={tab.id === view.active}
          disabled={!view.owner}
          title={tab.url}
          onclick={() => switchTab(tab.id)}
          >{tab.title || tab.url || 'New tab'}</button
        >
      {/each}
    </div>
    <div class="browser-toolbar">
      <span class="browser-address" title={selected?.url}
        >{selected?.url ?? ''}</span
      >
      <button
        class="btn"
        type="button"
        disabled={busy || !selected}
        onclick={() => void transfer()}
        title={view.owner
          ? 'Hand back (Shift+Esc)'
          : 'Take control of this browser'}
        >{view.owner ? 'Hand back' : 'Take over'}</button
      >
    </div>
  {/if}
  {#if error}<p class="browser-error" role="alert">{error}</p>{/if}
  <div class="browser-viewport" class:controlled={view?.owner}>
    {#if frame}
      <canvas
        bind:this={canvas}
        role={view?.owner ? 'application' : 'img'}
        aria-label={view?.owner
          ? 'Browser page. Shift+Escape hands back control.'
          : 'Live browser page'}
        tabindex={view?.owner ? 0 : undefined}
        onclick={click}
        onwheel={scroll}
        onkeydown={keydown}
        onpaste={paste}
      ></canvas>
    {:else if view?.active}
      <span>Waiting for a browser frame</span>
    {:else if view}
      <span>The agent hasn’t opened a browser yet.</span>
    {:else if !error}
      <span>Opening browser view…</span>
    {/if}
  </div>
</section>

<style>
  .thread-browser {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
  }
  .browser-tabs,
  .browser-toolbar {
    display: flex;
    align-items: center;
    gap: var(--browser-toolbar-gap);
    padding: var(--browser-toolbar-padding);
    border-bottom: var(--browser-toolbar-border);
  }
  .browser-tabs {
    overflow-x: auto;
  }
  .browser-tabs .btn {
    flex: 0 0 auto;
  }
  .browser-tabs .btn[aria-pressed='true'] {
    border-color: var(--accent-line);
    background: var(--accent-soft);
  }
  .browser-address {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-2);
    font-family: var(--mono);
  }
  .browser-viewport {
    flex: 1;
    display: grid;
    place-items: center;
    min-height: 0;
    overflow: hidden;
    color: var(--muted);
    background: var(--bg);
  }
  .browser-viewport.controlled {
    box-shadow: var(--browser-controlled-outline);
  }
  canvas {
    max-width: 100%;
    max-height: 100%;
    min-height: 0;
    object-fit: contain;
  }
  .browser-error {
    color: var(--err);
    padding: var(--browser-toolbar-padding);
  }
</style>
