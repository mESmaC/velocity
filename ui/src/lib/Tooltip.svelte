<script lang="ts">
  import { onMount } from 'svelte';
  import { pop } from './motion';

  const HOVER_DELAY = 350;
  let anchor: Element | null = null;
  let originalTitle = '';
  let watchingTitle: globalThis.MutationObserver | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let shown = $state.raw<{ anchor: Element; text: string } | null>(null);
  let visible = $state(false);
  let tooltip = $state<HTMLElement>();

  function hide() {
    clearTimeout(timer);
    visible = false;
  }

  function show() {
    if (!anchor?.isConnected || !originalTitle || anchor.closest('[inert]'))
      return;
    shown = { anchor, text: originalTitle };
    visible = true;
  }

  function target(element: globalThis.EventTarget | null, delay: number) {
    const next = element instanceof Element ? element.closest('[title]') : null;
    if (next === anchor) return;
    hide();
    watchingTitle?.disconnect();
    // Blank the hovered title to suppress the WebView's own popup.
    // Restore it after hover so it still supplies the native description.
    if (anchor?.getAttribute('title') === '')
      anchor.setAttribute('title', originalTitle);
    anchor = next;
    originalTitle = anchor?.getAttribute('title') ?? '';
    if (!originalTitle) return;
    if (delay && anchor) suppressNativeTitle(anchor);
    timer = setTimeout(show, delay);
  }

  function suppressNativeTitle(element: Element) {
    element.setAttribute('title', '');
    watchingTitle = new window.MutationObserver(() => {
      const title = element.getAttribute('title');
      if (title === '') return;
      if (!title) {
        target(null, 0);
        return;
      }
      originalTitle = title;
      element.setAttribute('title', '');
      if (visible) show();
    });
    watchingTitle.observe(element, { attributeFilter: ['title'] });
  }

  function pointer(event: globalThis.PointerEvent) {
    if (event.pointerType !== 'mouse') return;
    target(
      event.type === 'pointerout' ? event.relatedTarget : event.target,
      HOVER_DELAY,
    );
  }

  function focus(event: globalThis.FocusEvent) {
    if (
      event.target instanceof Element &&
      event.target.matches(':focus-visible')
    )
      target(event.target, 0);
  }

  function blur(event: globalThis.FocusEvent) {
    if (
      anchor?.contains(
        event.target instanceof window.Node ? event.target : null,
      ) &&
      anchor.getAttribute('title') !== ''
    )
      target(null, 0);
  }

  function overlayClosing(event: globalThis.Event) {
    if (event.target instanceof Element && event.target.contains(anchor))
      target(null, 0);
  }

  $effect(() => {
    if (!tooltip || !shown || !visible) return;
    const host = shown.anchor.closest('dialog:modal') ?? document.body;
    if (tooltip.parentElement !== host) host.append(tooltip);
    if (!tooltip.matches(':popover-open')) tooltip.showPopover();
    const { left, top, bottom } = shown.anchor.getBoundingClientRect();
    tooltip.style.left = `${Math.max(0, Math.min(left, window.innerWidth - tooltip.offsetWidth))}px`;
    tooltip.style.top = `${
      bottom + tooltip.offsetHeight <= window.innerHeight
        ? bottom
        : Math.max(0, top - tooltip.offsetHeight)
    }px`;
  });

  onMount(() => {
    // Svelte's transition events do not bubble out of their owning overlay.
    document.addEventListener('outrostart', overlayClosing, true);
    document.addEventListener('scroll', hide, true);
    return () => {
      document.removeEventListener('outrostart', overlayClosing, true);
      document.removeEventListener('scroll', hide, true);
      target(null, 0);
    };
  });
</script>

<svelte:document
  onpointerover={pointer}
  onpointerout={pointer}
  onpointerdown={hide}
  onfocusin={focus}
  onfocusout={blur}
  onkeydown={(event) => {
    if (event.key === 'Escape') hide();
  }}
/>
<svelte:window onblur={() => target(null, 0)} onresize={hide} />

{#if visible && shown}
  <div
    class="tooltip"
    role="tooltip"
    popover="manual"
    bind:this={tooltip}
    transition:pop={{ style: 'fade' }}
  >
    {shown.text}
  </div>
{/if}

<style>
  .tooltip {
    inset: auto;
    margin: 0;
    max-width: 100vw;
    max-height: 100vh;
    transform: none;
    overflow-wrap: anywhere;
  }
</style>
