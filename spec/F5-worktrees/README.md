<!-- split-spec: {"order":523,"kind":"markdown","indentation":0} -->
### F5 · Workspaces

**Rosen workspaces (decided, #331, #407, #418, #419).**
Rosen (`github.com/mESmaC/rosen`) is Velocity's only version control.
Each project's agents work in Rosen changes and workspaces.
A project never creates or falls back to a git worktree or git branch when its server or workspace creation fails (#407, #418).
Legacy git projects migrate automatically at startup (#419); new threads wait until migration finishes.

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
- [Pin bumps (#513)](rosen-pin-bump.md)
- [Base CI (#507)](rosen-base-ci.md)
- [Project health checks (#509)](project-health.md)
- [Path](path.md)
- [Settings → Workspaces](settings-worktrees.md)
- [Warm pool (speed)](warm-pool.md)
- [Guards](guards.md)
- [Accept when](accept-when.md)
