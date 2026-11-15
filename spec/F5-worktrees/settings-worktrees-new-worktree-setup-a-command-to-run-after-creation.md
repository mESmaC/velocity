#### Settings → Worktrees · New worktree setup · a command to run after creation, saved per project (empty by default; Velocity itself can use pnpm -C ui install)

<!-- split-spec: {"order":657,"kind":"markdown","indentation":2} -->
- a command to run after creation, saved per project (empty by default; Velocity itself can use `pnpm -C ui install`).
  Clearing it skips the command.
  Older saved global commands remain a fallback, except the former built-in `pnpm -C ui install` default, which is ignored until explicitly set for a project.
