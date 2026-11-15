#### PR duty · Checks before merge · Local checks

<!-- split-spec: {"order":980,"kind":"markdown","indentation":2} -->
- **Local checks** are how these are reviewed instead: a sensitive PR (Tiered review), a PR that changes a file in the Lake project, and a PR whose head has no GitHub check, because nothing else would build it.
  The agent runs the project's checks in its review worktree on the PR merged onto the current `master` (the commands come from AGENTS.md), then approves and merges itself through the landing steps in F6.
  A pending, red or stale GitHub run does not hold such a PR up.
