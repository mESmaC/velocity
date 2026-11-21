import type { TranscriptBlock } from './ipc';

/** Minimum block count before windowing engages; short transcripts render directly. */
export const VIRTUALIZE_THRESHOLD = 32;

/** Extra pixels kept mounted above and below the viewport so fast wheel scrolling stays smooth. */
export const OVERSCAN_PX = 900;

/** Default vertical flex gap matching `--sp-6` (16px) in `.transcript-inner`. */
export const DEFAULT_GAP_PX = 16;

export interface VisibleRange {
  start: number;
  end: number;
  topPad: number;
  bottomPad: number;
}

/** Rough initial pixel height before ResizeObserver measures a mounted block. */
export function estimateBlockHeight(block: TranscriptBlock): number {
  if (block.StatusLine) return 28;
  if (block.UserMessage) {
    const lines = Math.max(1, Math.ceil(block.UserMessage.text.length / 64));
    return Math.min(480, lines * 22 + 28 + (block.UserMessage.queued ? 24 : 0));
  }
  if (block.Notice) {
    const lines = Math.max(1, Math.ceil(block.Notice.text.length / 64));
    return Math.min(480, lines * 22 + 44);
  }
  if (block.AgentMessage) {
    let chars = 0;
    for (const paragraph of block.AgentMessage.paragraphs) {
      chars += paragraph.length;
    }
    const lines = Math.max(1, Math.ceil(chars / 78));
    return Math.min(1200, lines * 22 + 20);
  }
  if (block.ToolGroup) {
    return block.ToolGroup.steps.length * 26 + 18;
  }
  if (block.Diff) {
    return Math.min(1400, block.Diff.block.lines.length * 20 + 46);
  }
  if (block.ErrorCard) return 96;
  if (block.Decision) return 84;
  return 64;
}

/**
 * Computes the slice of `[start, end)` to mount and the top/bottom padding in pixels
 * using binary search over prefix-summed item heights (#553).
 */
export function computeVisibleRange(
  blocks: TranscriptBlock[],
  ids: readonly number[],
  measuredHeights: ReadonlyMap<number, number>,
  scrollTopInList: number,
  viewportHeight: number,
  gap: number = DEFAULT_GAP_PX,
): VisibleRange {
  const count = blocks.length;
  if (count === 0) {
    return { start: 0, end: 0, topPad: 0, bottomPad: 0 };
  }

  const offsets = new Float64Array(count + 1);
  for (let index = 0; index < count; index += 1) {
    const id = ids[index];
    const height =
      (id !== undefined ? measuredHeights.get(id) : undefined) ??
      estimateBlockHeight(blocks[index]);
    const step = index + 1 < count ? height + gap : height;
    offsets[index + 1] = offsets[index] + step;
  }

  const totalHeight = offsets[count];
  const minTop = Math.max(0, scrollTopInList - OVERSCAN_PX);
  const maxBottom =
    scrollTopInList + Math.max(viewportHeight, 400) + OVERSCAN_PX;

  // Binary search for first block whose bottom edge exceeds minTop.
  let low = 0;
  let high = count - 1;
  while (low < high) {
    const mid = (low + high) >>> 1;
    if (offsets[mid + 1] <= minTop) {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  const start = low;

  // Binary search for first block whose top edge exceeds maxBottom.
  low = start;
  high = count;
  while (low < high) {
    const mid = (low + high) >>> 1;
    if (offsets[mid] <= maxBottom) {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  const end = Math.max(start + 1, low);

  const topPad = offsets[start];
  const bottomPad = Math.max(0, totalHeight - offsets[end]);
  return { start, end, topPad, bottomPad };
}

export interface HeightTracker {
  readonly heights: ReadonlyMap<number, number>;
  observe(node: HTMLElement, id: number): void;
  updateId(node: HTMLElement, id: number): void;
  unobserve(node: HTMLElement): void;
  clear(): void;
  destroy(): void;
}

/**
 * Tracks rendered block heights via a single shared ResizeObserver and preserves scroll
 * anchor position when an above-viewport block is measured while scrolled up (#553).
 */
export function createHeightTracker(
  getScrollContainer: () => HTMLElement | null,
  onHeightChange: () => void,
): HeightTracker {
  const heights = new Map<number, number>();
  const elements = new Map<Element, number>();
  const observer = new globalThis.ResizeObserver((entries) => {
    let changed = false;
    let anchorDelta = 0;
    const container = getScrollContainer();
    const containerTop = container ? container.getBoundingClientRect().top : 0;
    const atBottom =
      container !== null &&
      container.scrollHeight - container.clientHeight - container.scrollTop <=
        4;

    for (const entry of entries) {
      const id = elements.get(entry.target);
      if (id === undefined) continue;
      const nextHeight = entry.contentRect.height;
      if (nextHeight <= 0) continue;
      const previousHeight = heights.get(id);
      if (
        previousHeight === undefined ||
        Math.abs(previousHeight - nextHeight) >= 0.5
      ) {
        if (
          container &&
          !atBottom &&
          previousHeight !== undefined &&
          entry.target.getBoundingClientRect().bottom <= containerTop
        ) {
          anchorDelta += nextHeight - previousHeight;
        }
        heights.set(id, nextHeight);
        changed = true;
      }
    }
    if (container && Math.abs(anchorDelta) >= 1) {
      container.scrollTop += anchorDelta;
    }
    if (changed) {
      onHeightChange();
    }
  });

  return {
    heights,
    observe(node, id) {
      elements.set(node, id);
      observer.observe(node);
    },
    updateId(node, id) {
      elements.set(node, id);
    },
    unobserve(node) {
      elements.delete(node);
      observer.unobserve(node);
    },
    clear() {
      heights.clear();
    },
    destroy() {
      observer.disconnect();
      elements.clear();
    },
  };
}

/** Observes scroll and viewport resize on `container` using requestAnimationFrame. */
export function observeScrollContainer(
  container: HTMLElement,
  onMetrics: () => void,
): () => void {
  let raf = 0;
  const run = () => {
    raf = 0;
    onMetrics();
  };
  const schedule = () => {
    if (!raf) raf = window.requestAnimationFrame(run);
  };
  onMetrics();
  container.addEventListener('scroll', schedule, { passive: true });
  const resizeObserver = new globalThis.ResizeObserver(schedule);
  resizeObserver.observe(container);
  return () => {
    if (raf) window.cancelAnimationFrame(raf);
    container.removeEventListener('scroll', schedule);
    resizeObserver.disconnect();
  };
}
