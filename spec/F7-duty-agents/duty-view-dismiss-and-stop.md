#### Duty view · Dismiss and Stop

<!-- split-spec: {"order":909,"kind":"markdown","indentation":0} -->
- **Dismiss and Stop.**
  Each agent card has a × (shown on hover or focus) that dismisses that agent at once, with no confirmation (§1).
  Its running session stops, which ends its agent's process tree; a PR it was reviewing is released (the AIRC `released` event, the `reviewing` label removed if an agent added one, and a `Released · <nick>` comment on the PR) and doesn't rest, so the next idle PR duty agent can claim it on its next tick.
  The agent leaves the role in `state.json` and the feed gets "Dismissed <nick>".
  **Stop** in the header dismisses every agent and removes the role from "On duty"; the view goes Home, as it does for a deleted thread.
  Both survive a restart.
  A later + Agent takes the lowest free nick numbers, so a dismissed agent's nick can be reused.
  Worktrees the agent used stay on disk.
  There is no Undo: + Agent or starting the role again brings agents back.
