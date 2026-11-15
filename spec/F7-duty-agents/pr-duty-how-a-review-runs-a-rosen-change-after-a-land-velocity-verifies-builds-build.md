#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests · build

<!-- split-spec: {"order":1026,"kind":"markdown","indentation":6} -->
- Rosen attempts to revert a confirmed **build** failure on the base and reopen its change.
  The feed names it, its thread goes back to review, stage fixing, with a workspace, the command, its exit status and the end of its output (F5 "Followed through Rosen's events"), and no other change is touched.
  Reverts never rewrite history or discard the author's work.
  The revert is a land: the next run checks it.
  A requested inverse is not reported as completed until Rosen's durable failure record names its revert commit.
  If Rosen reports `revert_refused`, Velocity immediately starts one fix-forward build thread for the culprit on the default implementation model (#406).
  A culprit still unreverted two minutes after its recorded failure gets the same repair at that deadline, even if no PR duty agents are staffed or their role is paused.
  The prompt includes the failing check output, culprit change and commit, the later commits blocking the inverse, and the instruction to make the base build again and touch nothing else.
  Fix-forward scheduling runs independently of verification so a running build or another repair never holds it up.
  The culprit-to-thread association and opening prompt are saved together before launch; retries resume that pending launch, and restarts or deleted history never allocate a second thread for the same culprit.
  Each new build culprit gets its own repair thread.
  The repair lands like any change, and the next successful post-land build covering the culprit closes its record; a completed revert also closes it.
  Test success alone never closes a build culprit.
