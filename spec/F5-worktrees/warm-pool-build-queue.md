#### Warm pool (speed) · Build queue

<!-- split-spec: {"order":728,"kind":"markdown","indentation":0} -->
- **Build queue** *(proposed)*.
  Velocity runs agents' heavy builds (compile, test, lint, the PR duty landing check) through one machine-wide queue, one build at a time at full machine resources.
  Builds run side by side split the cores and memory and all finish late together; run one at a time, the first in line finishes in a fraction of the time and the average wait drops.
  Order: landing checks first, then checks before a PR is marked ready, then everything else, first come, first served within each.
  A build a change can't affect never enters the queue: a docs-only change builds nothing, and a change to one part (the UI, one crate or package, the proofs) runs that part's checks, not the whole project's.
  Agents are told the same (F11 default AGENTS.md): build only what the change needs, and never rerun a build or check when nothing it covers changed since it passed.
  The footer shows the running build and the queue.
  Long-running dev servers never hold the queue.
  Until Velocity runs builds itself, `scripts/build-queue.ps1` is the stand-in (`-Priority landing|ready|normal`; `scripts/check.ps1` passes `ready` by default, PR duty passes `landing`).
  On a machine with Rosen it takes its turn in Rosen's run queue (Rosen R3), the one line Velocity's pre- and post-land checks, Rosen's warm pool prepares and agents' `rosen run` builds already wait in: `landing` runs in Rosen's `landing` class, `ready` and `normal` in `build`.
  The rosen program is the one `VELOCITY_ROSEN_BIN` names, else the first on `PATH`.
  Beneath a turn (`ROSEN_RUN_HELD` set, as Velocity's checks set it on their commands) it runs the command at once, since it is part of that turn.
  Without Rosen it keeps its own line of tickets and a named machine-wide lock, both released by Windows when their holder exits, so a crashed build or a killed waiter never holds up the line.
  Agents of every project go through the one line, whatever their AGENTS.md says (#308): every agent Velocity starts outside a Rosen workspace, such as one in a git worktree, gets Rosen's folder first on its `PATH` and a note ahead of its first prompt, not shown in the transcript, telling it to run every heavy build, test and lint through `rosen run -- <command>` (a landing check with `--class landing`) in place of any build queue script its instructions name.
  `rosen run` waits in the machine's run queue whatever folder it runs in.
  The shared ACP client supplies the note to every prompted agent, including spec interviews, AGENTS.md rewrites and thread-title helpers, on initial launch and resume.
  Rosen's folder is also included when resolving executables and starting ACP terminals: a provider using Velocity's `terminal/create` capability can run the same command as one using its own shell, even when Rosen is bundled rather than installed on the user's PATH.
  A terminal with its own PATH keeps its entries after Rosen's folder.
  Queue discovery requires only the Rosen CLI, so git projects can use it without installing or starting a Rosen server.
  A tool call running `rosen run` counts as waiting in the build queue, so it is never stalled.
  Nothing changes when the `rosen` program isn't found.

<!-- split-spec links -->

Rule files:

- [The run queue's classes overlap on purpose](warm-pool-build-queue-the-run-queue-s-classes-overlap.md)
