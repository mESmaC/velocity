/**
 * Returns `next`, but with every part equal to the same part of `previous` replaced by
 * `previous`'s object. Each snapshot from Rust is a fresh object tree, so without this
 * every block of a long transcript looked changed to Svelte on every snapshot and was
 * updated again (#321). Unchanged parts keep their identity, so only what changed redraws.
 */
export function reuseUnchanged<Value>(previous: Value, next: Value): Value {
  return reuse(previous, next) as Value;
}

function reuse(previous: unknown, next: unknown): unknown {
  if (previous === next) return previous;
  if (
    typeof previous !== 'object' ||
    typeof next !== 'object' ||
    previous === null ||
    next === null
  )
    return next;
  if (Array.isArray(previous) !== Array.isArray(next)) return next;
  if (Array.isArray(previous) && Array.isArray(next)) {
    return reuseArray(previous, next);
  }
  return reuseRecord(
    previous as Record<string, unknown>,
    next as Record<string, unknown>,
  );
}

function reuseArray(previous: unknown[], next: unknown[]): unknown[] {
  let same = previous.length === next.length;
  const merged = next.map((item, index) => {
    const kept = index < previous.length ? reuse(previous[index], item) : item;
    if (kept !== previous[index]) same = false;
    return kept;
  });
  return same ? previous : merged;
}

function reuseRecord(
  previous: Record<string, unknown>,
  next: Record<string, unknown>,
): Record<string, unknown> {
  const keys = Object.keys(next);
  let same = keys.length === Object.keys(previous).length;
  const merged: Record<string, unknown> = {};
  for (const key of keys) {
    const kept = key in previous ? reuse(previous[key], next[key]) : next[key];
    if (kept !== previous[key]) same = false;
    merged[key] = kept;
  }
  return same ? previous : merged;
}
