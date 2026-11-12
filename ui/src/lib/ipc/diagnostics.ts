import { commands } from '../bindings';

// Errors the window catches go to velocity.log in the app data folder (#273), so a blank or
// broken window leaves evidence even in a release build with no devtools.
const recent = new Map<string, number>();
const REPEAT_INTERVAL_MS = 10_000;

function describe(error: unknown): string {
  if (error instanceof Error)
    return error.stack?.includes(error.message)
      ? error.stack
      : `${error.name}: ${error.message}\n${error.stack ?? ''}`;
  if (typeof error === 'string') return error;
  try {
    return JSON.stringify(error) ?? String(error);
  } catch {
    return String(error);
  }
}

/** Logs an error the window caught. A pane that fails on every update logs once per interval. */
export function reportWindowError(source: string, error: unknown) {
  const message = describe(error);
  console.error(`[${source}]`, error);
  const key = `${source}\n${message}`;
  const now = Date.now();
  const last = recent.get(key);
  if (last !== undefined && now - last < REPEAT_INTERVAL_MS) return;
  recent.set(key, now);
  if (recent.size > 100) recent.clear();
  // Logging must never raise a second error of its own.
  commands.logWindowError(source, message).catch(() => undefined);
}
