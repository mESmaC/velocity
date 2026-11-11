// Svelte transitions for the motion system (#226, SPEC.md §6 Motion). The
// durations, curves, travel and scale come from the --motion-* tokens in
// theme/reference.css, read when each transition starts, so CSS and Svelte
// move the same way and prefers-reduced-motion (which changes the tokens)
// applies to both. Things arrive with the base duration and an ease-out, and
// leave with the quick duration and an ease-in.
import type { TransitionConfig } from 'svelte/transition';

type Direction = { direction: 'in' | 'out' | 'both' };

type Motion = {
  duration: number;
  easing: (time: number) => number;
  rise: number;
  scale: number;
};

function readMotion(arriving: boolean, durationToken: string): Motion {
  const style = getComputedStyle(document.documentElement);
  const read = (name: string) => style.getPropertyValue(name).trim();
  return {
    duration: milliseconds(
      read(arriving ? durationToken : '--motion-duration-quick'),
    ),
    easing: cubicBezier(
      read(arriving ? '--motion-ease-out' : '--motion-ease-in'),
    ),
    rise: Number.parseFloat(read('--motion-rise')) || 0,
    scale: Number.parseFloat(read('--motion-scale-from')) || 1,
  };
}

function milliseconds(value: string): number {
  const amount = Number.parseFloat(value);
  if (!Number.isFinite(amount)) return 0;
  return value.endsWith('ms') ? amount : amount * 1000;
}

// Parses `cubic-bezier(x1, y1, x2, y2)` into an easing function. Svelte
// transitions need a JavaScript curve, and the token is the single source.
function cubicBezier(value: string): (time: number) => number {
  const points = /cubic-bezier\(([^)]*)\)/
    .exec(value)?.[1]
    ?.split(',')
    .map((part) => Number.parseFloat(part));
  if (!points || points.length !== 4 || points.some((n) => !Number.isFinite(n)))
    return (time) => time;
  const [x1, y1, x2, y2] = points as [number, number, number, number];
  const curve = (a: number, b: number, s: number) =>
    3 * a * s * (1 - s) ** 2 + 3 * b * s * s * (1 - s) + s ** 3;
  return (time) => {
    if (time <= 0 || time >= 1) return time;
    // Bisection finds the curve parameter whose x is `time`; 20 steps is
    // closer than a pixel for any duration used here.
    let low = 0;
    let high = 1;
    for (let step = 0; step < 20; step += 1) {
      const middle = (low + high) / 2;
      if (curve(x1, x2, middle) < time) low = middle;
      else high = middle;
    }
    return curve(y1, y2, (low + high) / 2);
  };
}

/** Menus, popovers and dialogs: fade and grow from --motion-scale-from. A
 *  dialog's ::backdrop follows through --motion-presence. A leaving menu
 *  takes no clicks, so a click meant for what is under it lands there.
 *  Modal dialogs use it only to arrive (`in:pop|global`, since several sit
 *  inside a {#key} that is created with them): a closing modal would keep
 *  focus and block the page for the length of the fade. */
export function pop(
  _node: Element,
  _parameters?: unknown,
  options?: Direction,
): TransitionConfig {
  const leaving = options?.direction === 'out';
  const motion = readMotion(!leaving, '--motion-duration-base');
  return {
    duration: motion.duration,
    easing: motion.easing,
    css: (time) =>
      `opacity: ${time}; transform: scale(${motion.scale + (1 - motion.scale) * time}); --motion-presence: ${time};${leaving ? ' pointer-events: none;' : ''}`,
  };
}

/** Rows, blocks and expanded details: fade and rise --motion-rise pixels. */
export function rise(
  _node: Element,
  _parameters?: unknown,
  options?: Direction,
): TransitionConfig {
  const motion = readMotion(
    options?.direction !== 'out',
    '--motion-duration-fast',
  );
  return {
    duration: motion.duration,
    easing: motion.easing,
    css: (time) =>
      `opacity: ${time}; transform: translateY(${(1 - time) * motion.rise}px);`,
  };
}
