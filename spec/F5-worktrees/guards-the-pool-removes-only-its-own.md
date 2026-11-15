#### Guards · The pool removes only its own ready worktrees

<!-- split-spec: {"order":759,"kind":"markdown","indentation":0} -->
- The pool removes only its own ready worktrees: a registered, detached checkout under a `.ready-<n>` name.
  A thread's worktree is never removed through it.
