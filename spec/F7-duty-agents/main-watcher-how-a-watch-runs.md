#### Main watcher · How a watch runs

<!-- split-spec: {"order":1077,"kind":"markdown","indentation":0} -->
- **How a watch runs.**
  Start it from the duty start dialog, the palette ("Put an agent on main watch…") or the + on the On duty group.
  It has no settings beyond its locked rules; the commands it runs come from AGENTS.md.

<!-- split-spec links -->

Rule files:

- [The runner reads the base branch's tip on the remote at most once a minute](main-watcher-how-a-watch-runs-the-runner-reads-the-base-branch.md)
- [A watch is one agent turn in the watcher's own <watcher>-sweep worktree (F5), a detached checkout of that tip, reset before each watch](main-watcher-how-a-watch-runs-a-watch-is-one-agent-turn.md)
- [The watcher records verdicts only through Velocity's watch MCP tools, served by Velocity's own executable with the watcher's nick and tip fixed by the launcher](main-watcher-how-a-watch-runs-the-watcher-records-verdicts-only-through.md)
- [Flaky tests are filed through the scouts' scout_issues and scout_file tools (never duplicates, failing evidence attached), signed with the watcher's nick](main-watcher-how-a-watch-runs-flaky-tests-are-filed-through-the.md)
- [A message from the Main watcher composer goes to the running watch as its next message; with none running, the next idle watcher watches the tip again with it](main-watcher-how-a-watch-runs-a-message-from-the-main-watcher.md)
