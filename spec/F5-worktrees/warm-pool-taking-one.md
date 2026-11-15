#### Warm pool (speed) · Taking one

<!-- split-spec: {"order":690,"kind":"markdown","indentation":0} -->
- **Taking one.**
  A new thread or agent takes a ready worktree, renames it to its path, and creates its branch there.
  If `master` has moved since the worktree was made, it fast-forwards first, which is cheap.
  The take is marked under the pool's lock, so two threads never get the same worktree.
  A take that fails falls back to a fresh checkout; no ready worktree, likewise.
