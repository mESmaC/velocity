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

  // Growth shows up as a resize of the content, which the browser reports after layout.
  // Watching every text change in the whole transcript instead forced a layout of all
  // of it for each streamed chunk (#321). The content element itself can be replaced,
  // so watch the viewport's direct children for that and observe the new one.
  const resizing = new ResizeObserver(follow);
  resizing.observe(viewport);
  let content = viewport.firstElementChild;
  if (content) resizing.observe(content);
  const mutations = new MutationObserver(() => {
    if (viewport.firstElementChild === content) return;
    if (content) resizing.unobserve(content);
    content = viewport.firstElementChild;
    if (content) resizing.observe(content);
    follow();
  });
  mutations.observe(viewport, { childList: true });
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
