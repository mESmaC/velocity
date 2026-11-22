#### Rosen · A thread's workspace

<!-- split-spec: {"order":542,"kind":"markdown","indentation":0} -->
- **A thread's workspace.**
  A new thread gets a Rosen change and a workspace at the folder its workspace would have had, from Rosen's warm pool when it has one ready (Rosen R3).
  Velocity starts each project's Rosen server with its warm pool in `.rosen-pool`, among the project's workspaces, because Rosen hands out a ready workspace by renaming it and that works only within one volume.
  Velocity keeps no git worktrees ready for a Rosen project.
  Once per run it raises Rosen's workspace disk budget to what is charged now plus 80% of the free space on that volume: Rosen's default of 100 GiB counts every pooled and taken workspace in full, and a request over it waits.
  Its agent's Rosen name is that folder's name.
  The first transcript step is `Workspace · <path> · Rosen change <id> from <base>`, using the project's base ref.
  Velocity creates the change first and saves it on the thread before any file is written, so a slow or failed workspace never leaves a change the thread does not know, and a retry reuses that change (#514).
  Reusing an existing workspace at the planned path requires its `.rosen/workspace.json` marker to name the thread's saved change; a folder carrying another change's marker is left intact and never enters setup (#659).
  While the workspace is still being made, the step reads `creating from <base>` followed by what the machine's run queue shows it waits on, such as `waiting for the warm pool: prepare running since 12:27` or `waiting for the run queue: <what the starving run waits for>`.
  After three minutes without a workspace Velocity starts a second attempt on a Rosen connection of its own, as `rosen change new --workspace` has, and takes whichever workspace is made first.
  After three more minutes without one the launch fails with that reason, the thread keeps its change, and **Retry Rosen workspace** tries again.
  Velocity then runs the per-agent setup steps below (untracked files, skills, compiler-cache environment and setup command), which Rosen leaves to whoever takes a workspace.
