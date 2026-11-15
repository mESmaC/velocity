<!-- split-spec: {"order":523,"kind":"markdown","indentation":0} -->
### F5 · Worktrees

**Rosen first, git as the fallback (decided, #331).**
Rosen (`github.com/mESmaC/rosen`) is Velocity's first-class version control.
Each project's agents work in Rosen changes and workspaces; git worktrees are the fallback, used when a project chooses git or Rosen can't make a workspace.
A project's choice is saved with it and defaults to Rosen.
Settings → Worktrees → Version control switches it for the selected project (`set_worktree_settings` with `UseRosen`).
Only new threads take the new choice: an open thread keeps the Rosen change or git worktree and PR it started with until it lands, and the page says how many do.
A project that switches to git keeps its Rosen event follower and post-land checks while any of its threads has a Rosen change, so a change that landed before the switch is still verified, and reverted and reopened if it breaks.
The ready worktree pool fills for a project once it uses git, and stops for one that uses Rosen.
The rest of this section describes git worktrees, which still back every job that has not moved to Rosen yet.

<!-- split-spec links -->

Rule files:

- [The project's server](rosen-the-project-s-server.md)
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
- [Settings → Worktrees](settings-worktrees.md)
- [Warm pool (speed)](warm-pool.md)
- [Guards](guards.md)
- [Accept when](accept-when.md)
