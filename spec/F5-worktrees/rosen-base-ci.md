#### Rosen · Base CI (#507)

Lands go through after-land review, and the integration build runs on one machine.
A job that fails only on GitHub's runners, such as a macOS or Linux job, is found by reading GitHub CI on the base.
Velocity does this for every Rosen project that has a GitHub repository, with `gh` signed in as the owner.
It needs no thread, agent session or window.

**The look.**
Ninety seconds after start and every three minutes, Velocity reads the newest 50 workflow runs on the project's base branch in its GitHub repository, and the commit statuses on that branch's tip.
A run counts when GitHub started it for a base commit: its event is `push` or `schedule`.
A dispatched run reports to whoever asked for it (#508), and a pull request's run is not on the base.
A commit status counts as a workflow named for its context, except `velocity/integration`, which is Velocity's own integration build.

| Run or status | What it says |
|---|---|
| completed with `success` | passed |
| completed with `failure`, `timed_out` or `startup_failure`; a status of `failure` or `error` | failed |
| queued or in progress; a status of `pending` | running |
| anything else, such as `cancelled` or `skipped` | nothing |

The newest run of each workflow that passed or failed decides that workflow.
An older failure under a newer pass is fixed, a run still going changes nothing until it finishes, and a cancelled run is never a failure.
For a workflow whose deciding run failed, Velocity reads that run's jobs.
A job failed when it concluded `failure` or `timed_out`; a job cancelled because another failed did not.
A failed run with no failed job, and a failed status, is one failure named for the workflow.

**The fix-forward thread.**
A failed job on the base is treated like a confirmed integration failure, whichever operating system it ran on.
Velocity keeps one open repair per workflow and job.
A failed job with no open repair gets a fix-forward thread; the jobs that newly failed in one run share one thread, since one fault often fails every cell of a matrix.
The thread is an ordinary build thread on the default implementation model, titled "Fix CI: `<workflow>` · `<job>`", and the project's spec must be locked.
Its prompt names the repository, branch, workflow, commit and run, and for up to three jobs the job, its first failed step, its runner and an excerpt of that job's failed-step log.
Other failed jobs are listed by name.
The run's whole failed-step log is saved as `velocity-base-ci-<run>/failed-steps.log` in the temporary folder, and the prompt gives its path.
The prompt tells the agent to work from the log because the runner may be an operating system this machine cannot run, to check that the base still has the fault, and never to weaken the failing test or lint.
The same job failing again while its repair is open starts nothing.
The repair and its thread's opening prompt are saved together before the thread starts, so a restart resumes the same thread and never starts a second one.
A repair stays open when its thread is deleted.

**Closing.**
A workflow whose deciding run passed has its repairs closed, so the fix landing closes them with no manual step.
Only a green run of the same workflow closes a repair.
A later failure of that job is then a new failure with its own thread.

**What the project shows.**
The Rosen status pane's Base section (F6) shows the base's CI state beside the last green integration commit.

| State | When | Line |
|---|---|---|
| failing | a deciding run failed | "GitHub CI failing at `<commit>`", in the error colour, with each failing job as `<workflow> · <job>`, and "new run in progress" while a newer run is going |
| running | nothing failed and a run is queued or in progress | "GitHub CI running" |
| green | every deciding run passed | "GitHub CI green at `<commit>`" |
| not run | no run has finished and none is going | "GitHub CI has not run" |

The commit is the base commit of the newest deciding run.
The state is saved with the project and shown at startup before the next look.
A toast says when a fix-forward thread starts and when the base turns green after failing.

**Failures.**
A look that cannot read GitHub changes nothing and is retried at the next look.
Each thread started, each repair closed and each failed look is written to `velocity.log` under `base ci`; a failure that repeats is written once.
