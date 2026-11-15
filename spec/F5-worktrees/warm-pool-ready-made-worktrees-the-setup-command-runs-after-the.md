#### Warm pool (speed) · Ready-made worktrees · The setup command runs after the take

<!-- split-spec: {"order":686,"kind":"markdown","indentation":2} -->
- **The setup command runs after the take**, at the thread's own path *(proposed, #128)*.
  On Windows `pnpm install` links packages with absolute junctions, which point back into the pool folder once the worktree moves, and the next `pnpm install` there aborts.
  A cargo `target` folder has no such paths and stays fresh across the move.
