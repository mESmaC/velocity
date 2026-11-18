<!-- split-spec: {"order":523,"kind":"markdown","indentation":0} -->
### F5 · Workspaces

**Rosen workspaces (decided, #331, #407, #419).**
Rosen (`github.com/mESmaC/rosen`) is Velocity's first-class version control.
Each project's agents work in Rosen changes and workspaces after the project migrates.
A Rosen project never falls back to git when its server or workspace creation fails (#407).
Legacy projects migrate automatically at startup after their running threads finish.
The Git workspace pool serves legacy projects while their migration is pending.
Before creating or taking a git worktree or refilling its ready pool, a project Rosen has imported copies Rosen's landed history into its git repository and fast-forwards its default branch, including when there is no mirror.
A newer fetched remote tip or local base is retained when it contains that landed history; an offline mirror does not prevent using Rosen's local base.
If the base has diverged, the launch or pool refresh refuses with the conflicting commits and an explanation; it never resets the branch or discards uncommitted files.
An existing ready slot catches up to the synchronized tip before a new thread takes it.

<!-- split-spec links -->

Rule files:

- [The project's server](rosen-the-project-s-server.md)
- [Headless machines](headless-machines.md)
- [A thread's workspace](rosen-a-thread-s-workspace.md)
- [Stored as the agent writes](rosen-stored-as-the-agent-writes.md)
- [What the agent is told](rosen-what-the-agent-is-told.md)
- [Deleting the thread](rosen-deleting-the-thread.md)
- [Followed through Rosen's events (F6)](rosen-followed-through-rosen-s-events.md)
- [Issues carried by Rosen changes (#386)](rosen-issues-carried-by-rosen-changes.md)
- [Rebases at the step boundary (#271, #398)](rosen-rebases-at-the-step-boundary.md)
- [Falling back](rosen-falling-back.md)
- [Lifecycle (decided)](lifecycle.md)
- [Path](path.md)
- [Settings → Workspaces](settings-worktrees.md)
- [Warm pool (speed)](warm-pool.md)
- [Guards](guards.md)
- [Accept when](accept-when.md)
