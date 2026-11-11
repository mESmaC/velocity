/** Follow rendered content (including late question cards) until the reader scrolls up. */
export function followTranscript(viewport: HTMLElement, identifier: string) {
  let currentIdentifier = identifier;
  let following = true;
  let frame = 0;
  let previousTop = viewport.scrollTop;

  function follow() {
    if (frame) return;
    frame = window.requestAnimationFrame(() => {
      frame = 0;
      if (following) viewport.scrollTop = viewport.scrollHeight;
      previousTop = viewport.scrollTop;
    });
  }

  function onScroll() {
    const remaining =
      viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop;
    if (viewport.scrollTop < previousTop) following = false;
    if (remaining <= 2) following = true;
    previousTop = viewport.scrollTop;
  }

  const mutations = new MutationObserver(follow);
  mutations.observe(viewport, {
    childList: true,
    subtree: true,
    characterData: true,
  });
  const resizing = new ResizeObserver(follow);
  resizing.observe(viewport);
  if (viewport.firstElementChild) resizing.observe(viewport.firstElementChild);
  viewport.addEventListener('scroll', onScroll, { passive: true });
  follow();
  return {
    update(nextIdentifier: string) {
      if (nextIdentifier === currentIdentifier) return;
      currentIdentifier = nextIdentifier;
      following = true;
      follow();
    },
    destroy() {
      mutations.disconnect();
      resizing.disconnect();
      viewport.removeEventListener('scroll', onScroll);
      window.cancelAnimationFrame(frame);
    },
  };
}
