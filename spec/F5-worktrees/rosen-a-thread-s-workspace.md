#### Rosen · A thread's workspace

<!-- split-spec: {"order":542,"kind":"markdown","indentation":0} -->
- **A thread's workspace.**
  A new thread gets a Rosen change and a workspace at the folder its worktree would have had, from Rosen's warm pool when it has one ready (Rosen R3).
  Velocity starts each project's Rosen server with its warm pool in `.rosen-pool`, among the project's workspaces, because Rosen hands out a ready workspace by renaming it and that works only within one volume.
  Velocity keeps no git worktrees ready for a Rosen project.
  Once per run it raises Rosen's workspace disk budget to what is charged now plus 80% of the free space on that volume: Rosen's default of 100 GiB counts every pooled and taken workspace in full, and a request over it waits.
  Its agent's Rosen name is that folder's name.
  The first transcript step is `Workspace · <path> · Rosen change <id> from master`.
  Velocity then runs the per-agent setup steps below (untracked files, skills, compiler-cache environment and setup command), which Rosen leaves to whoever takes a workspace.
