#### Rosen · Rebases at the step boundary (#271, #398)

<!-- split-spec: {"order":589,"kind":"markdown","indentation":0} -->
- **Rebases at the step boundary (#271, #398).**
  When a land moves the base under a change that overlaps it, Rosen rebases the change on the server (Rosen R2, R5).
  Velocity writes the rebased head into the workspace only at the agent's step boundary: between turns, or while no session runs, never under a running step.
  The boundary is reserved during application: new messages and notices to a live session stay queued, and an ended session cannot resume until it finishes.
  Its watcher stops, the head is written, and the watcher starts again.
  Velocity does not snapshot an unapplied workspace immediately before this write: a snapshot replaces an already conflicted file directly, so resending the old file could hide the markers before the agent sees them.
  Unstored edits or pending offline snapshots refuse application; the watcher stores them and application retries at a later boundary.
  Whether a rebase waits is read from Rosen (the change's head against the workspace's last stored tree) on the `rebased` event, at the end of each turn and every 30 seconds, so a refused rebase, or one that arrived while Velocity was closed, is applied at the next check.
  The bundled Rosen includes rosen#383: manual `rosen rebase` also works while the watcher is open, under the snapshot writer lock, and the watcher reloads its basis afterwards (#377).
  This replaces `airc_rebase` for Rosen threads.

<!-- split-spec links -->

Rule files:

- [AIRC rebase tool (#396)](rosen-rebases-at-the-step-boundary-airc-rebase-tool.md)
- [A conflict (#379, #390, #398)](rosen-rebases-at-the-step-boundary-a-conflict.md)
