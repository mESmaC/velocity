#### Warm pool (speed) · Ready-made worktrees · A ready worktree is a detached checkout at <project root>\.ready-<n>

<!-- split-spec: {"order":682,"kind":"markdown","indentation":2} -->
- A ready worktree is a detached checkout at `<project root>\.ready-<n>`.
  No thread slug starts with a dot, so it never takes a thread's name.
  A `velocity-ready` file in its git admin folder (outside the checkout) records its base commit and whether its prebuild finished, so after a restart finished ones rejoin the pool and interrupted ones are removed and made again.
