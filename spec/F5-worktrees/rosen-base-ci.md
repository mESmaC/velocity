#### Rosen · Base CI (#507)

Lands go through after-land review, and the integration build runs on one machine.
A job that fails only on GitHub's runners, such as a macOS or Linux job, is found by reading GitHub CI on the base.
Velocity does this for every Rosen project that has a GitHub repository, does not mirror to Collapse, and has Issues on GitHub, with `gh` signed in as the owner.
On a project whose `[mirror] remote` is Collapse, on a project with Issues on Collapse, or on a project with no GitHub repository, Velocity never follows GitHub Actions and watches Rosen's per-platform post-land failures and checkpoints directly instead, whatever the issue host (#591, #613).
Each unreverted, uncovered per-platform failure starts a fix-forward thread naming the platform, the culprit change and commit, and the failure output, and closes once that platform's post-land checkpoint covers the culprit.
It needs no thread, agent session or window.

**The look.**
Ninety seconds after start and every three minutes, Velocity reads the newest 50 workflow runs on the project's base branch in its GitHub repository, and the commit statuses and check runs on that branch's tip (#652).
A run counts when GitHub started it for a base commit: its event is `push` or `schedule`.
A dispatched run reports to whoever asked for it (#508), and a pull request's run is not on the base.
A commit status counts as a workflow named for its context, except `velocity/integration`, which is Velocity's own integration build.
A standalone GitHub App check run on that commit counts as a workflow keyed by its app and check name, keeping the newest check run when more than one shares that key (#652).
Check runs published by GitHub Actions itself are skipped here because `/actions/runs` already tracks them and excludes dispatched or pull-request runs (#652).

| Run or status | What it says |
|---|---|
| completed with `success` | passed |
| completed with `failure`, `timed_out` or `startup_failure`; a `cancelled` run with a timed-out job; a status of `failure` or `error` | failed |
| queued or in progress; a status of `pending` | running |
| anything else, such as a push-cancelled or `skipped` run | nothing |

Each job is decided by the newest run of its workflow in which that job finished (passed or failed), whether or not the rest of that run has finished (#587).
An older failure under a newer passing run or a newer run whose matching job already passed is resolved, and a run cancelled by a newer push is superseded rather than a failure.
GitHub reports a job that hits its `timeout-minutes` as `cancelled`, so for cancelled runs newer than a workflow's latest decided run Velocity inspects the run's jobs (#584).
A job failed when it concluded `failure` or `timed_out`, or when it concluded `cancelled` and timed out; a job cancelled because another failed or because a newer push superseded it did not.
A cancelled job counts as timed out when its check run has an annotation matching `has exceeded the maximum execution time`, or, if annotations cannot be read, when no newer run of the same workflow exists on the branch and the job ran at least as long as its `timeout-minutes` read from the workflow file at that commit.
For each workflow, Velocity reads the jobs of its newest failed run and of any newer in-progress runs (up to five per workflow), in addition to any newer cancelled runs checked for timeouts (#587).
A failed run with no failed job, a failed check run, and a failed status each count as one failure named for the workflow.
If a newer run's matching job is still queued or running, Velocity waits for that job to finish before starting or reopening a repair, while still showing the base as failing until the job passes (#587).

**The fix-forward thread.**
A failed job on the base is treated like a confirmed integration failure, whichever operating system it ran on.
Velocity keeps one open repair per workflow and job.
A failed job with no open repair gets a fix-forward thread; the jobs that newly failed in one run share one thread, since one fault often fails every cell of a matrix.
The thread is an ordinary build thread on the default implementation model, titled "Fix CI: `<workflow>` · `<job>`", and the project's spec must be locked.
Its prompt names the repository, branch, workflow, commit and run, and for up to three jobs the job, its first failed step, its runner and an excerpt of that job's failed-step log.
For a timed-out job, Velocity reads the job's log and the prompt names the test still running when the job stopped, taken from the last `has been running for over 60 seconds` line (#584).
Other failed jobs are listed by name.
The run's whole failed-step log is saved as `velocity-base-ci-<run>/failed-steps.log` in the temporary folder, and the prompt gives its path.
The prompt tells the agent to work from the log because the runner may be an operating system this machine cannot run, to check that the base still has the fault, and never to weaken the failing test or lint.
The same job failing again with the same error while its repair is open and unlanded starts nothing.
Each open repair records the failing commit, the first concrete error and file location from the job log as its signature, and the landed commit once its thread lands (#558).
A newer failing commit with a different error signature starts a new fix-forward even while an earlier repair is active.
When a repair's thread lands and a failing run covers a commit at or after its land, Velocity supersedes the old repair and starts a new fix-forward on the next look; while the failing run strictly predates the land, the repair stays open waiting for CI.
A repair whose thread finishes without landing is superseded by a newer failing commit; one whose thread ends in error or is deleted without landing stays open for the same error so deleting a thread does not immediately respawn it.
The repair and its thread's opening prompt are saved together before the thread starts, so a restart resumes the same thread and never starts a second one.

**Closing.**
A workflow whose deciding run passed has all of its repairs closed, so the fix landing closes them with no manual step.
A job that passes in a newer run closes open repairs for that job immediately, even while other jobs in the newer run are still queued or running (#587).
A landed repair whose job no longer fails in a run at or after its land is closed as superseded even if another job in that workflow failed.
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
