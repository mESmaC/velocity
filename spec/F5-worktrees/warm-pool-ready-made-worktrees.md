#### Warm pool (speed) · Ready-made worktrees

<!-- split-spec: {"order":677,"kind":"markdown","indentation":0} -->
- **Ready-made worktrees.**
  Velocity keeps a few worktrees ready at the tip of `master` with setup already done: files copied, shared build cache linked, the project prebuilt.
  The count is 4 by default and set by "Keep N worktrees ready" on this page; 0 turns the pool off.
  Only projects whose spec is locked keep a pool (Rule 2).

<!-- split-spec links -->

Rule files:

- [A ready worktree is a detached checkout at <project root>\.ready-<n>](warm-pool-ready-made-worktrees-a-ready-worktree-is-a-detached.md)
- [The setup command runs after the take](warm-pool-ready-made-worktrees-the-setup-command-runs-after-the.md)
