#### Review view · Dismiss and Stop

<!-- split-spec: {"order":909,"kind":"markdown","indentation":0} -->
- **Dismiss and Stop.**
  Each agent card has a × (shown on hover or focus) that dismisses that agent at once, with no confirmation (§1).
  The running session stops and an active Rosen review claim is released.
  The agent leaves the role in `state.json` and the feed gets "Dismissed <nick>".
  **Stop** in the header dismisses every agent and removes the role from **Review**; the view goes Home, as it does for a deleted thread.
  Both survive a restart.
  A later + Agent takes the lowest free nick numbers, so a dismissed agent's nick can be reused.
  Workspaces the agent used stay on disk.
  There is no Undo: + Agent or starting the role again brings agents back.
