#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests · build

<!-- split-spec: {"order":1026,"kind":"markdown","indentation":6} -->
- A confirmed **build** failure is reverted on the base by Rosen and reopened.
  The feed names it, its thread goes back to review, stage fixing, with a workspace, the command, its exit status and the end of its output (F5 "Followed through Rosen's events"), and no other change is touched.
  Reverts never rewrite history or discard the author's work.
  The revert is a land: the next run checks it.
