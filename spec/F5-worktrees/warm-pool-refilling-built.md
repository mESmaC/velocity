#### Warm pool (speed) · Refilling · Built (proposed)

<!-- split-spec: {"order":704,"kind":"markdown","indentation":2} -->
- Built *(proposed)*: one worker makes, removes and catches up ready worktrees one at a time.
  It fetches the base branch every minute and at once when Velocity sees a merge, and uses `origin`'s tip when it can be fetched.
  A second worker prebuilds one worktree at a time, below normal priority so agents' own builds go first: `cargo build --workspace` in the worktree's own `target` folder, when the checkout is a Cargo project and "share the Rust build cache" is on.
  A prebuild that fails is not retried until `master` moves.
  Setup re-runs at every take, so no manifest check is needed.
