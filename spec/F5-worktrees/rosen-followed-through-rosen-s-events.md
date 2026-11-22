#### Rosen · Followed through Rosen's events (F6)

<!-- split-spec: {"order":566,"kind":"markdown","indentation":0} -->
- **Followed through Rosen's events (F6).**
  Velocity follows each Rosen project's event stream (Rosen R9) in place of the GitHub PR watcher, which no longer follows Rosen threads.
  A named snapshot (`rosen commit -m`) moves the thread to review.
  A land marks it landed and ticks off its tasks; its agent and watcher stop and its AIRC claims are released, and Rosen deletes the workspace.
  A post-land revert (Rosen R12) puts it back in review, stage fixing, reopens its tasks and its SPEC section as its open work until it lands again, gives it a workspace again at the same folder, at the reopened change's head, lets its thread run sessions again (the land had stopped them for good), and resumes its agent with what the failing checks printed (F7 "After a land").
  A `revert_refused` event for a build culprit starts a fix-forward thread immediately; a build culprit still unreverted two minutes after Rosen recorded it starts one at that deadline (#406).
  The thread uses the default implementation model and receives the failing check output, culprit change and commit, and the later commits Rosen reports as blocking the inverse, with the instruction to make the base build again and touch nothing else.
  Each culprit has at most one fix-forward thread, including after restarts and deletion of its thread history; another culprit gets its own thread even while a repair runs.
  A repair uses the normal Rosen workspace and landing lifecycle, and its culprit record closes when a subsequent successful integration build covers that culprit, or a legacy build repair completes its revert.
  Velocity lands the named change as soon as it combines cleanly with the base (F7).
  The note a Rosen workspace's agent gets says the same.
  Velocity saves the stream's cursor with each event that moves a thread, together with what the event leaves to do (a land's cleanup; a revert's workspace and message; a request for changes' message), and removes that work only once it is done.
  The cursor belongs to the server database that issued it, and Velocity logs and replays from zero if the database changes, the cursor is beyond the latest event, or an old save lacks the database identity (#482).
  Recovery skips superseded lifecycle events and preserves effects already recorded, including a landed thread's completed land cleanup.
  Before running pending work it flushes that snapshot to disk even when ordinary saves are deferred; a failed save or workspace lookup keeps cleanup pending (#353).
  A crash or a failed step therefore never acknowledges unfinished work: it is redone when the follower starts or reconnects and every half minute, and a message already in the thread's saved transcript is not sent again.
  Every Rosen project with a thread that has a change is followed, landed threads included, so a revert after a restart is heard even when nothing else in the project is open.
  A change lookup that returns `NotFound` retires the change reference of a done, abandoned or landed thread.
