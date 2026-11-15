<!-- split-spec: {"order":788,"kind":"markdown","indentation":0} -->
### F6 · PR follow-through

When the task is done, the agent opens a PR.
The PR description links the Task List item or says `Fixes #n`.
The thread moves to **PR open · watching**.
A thread in a Rosen workspace opens no PR: its project's Rosen events follow it instead (F5).

Draft claims keep the coding transcript until the PR is ready.
The watcher follows the thread's same-repository branch and excludes historical PRs from earlier uses of that branch.
New review work is saved before delivery and remains pending until its ACP prompt completes; after a restart it resumes in the retained worktree.
The PR author's own progress replies remain in history without prompting another response.
Merge cleanup and explicit deletion (F12) share the thread's lifecycle lock and wait for confirmed shutdown of its agent and owned terminals before removing the checkout.

Merge cleanup first verifies the registered worktree through F5's ownership and F17's deletion gate, including uncommitted, untracked and unpushed work.
It saves `worktree_removal_started` beside `worktree_deleted` in `state.json` before starting forced removal, flushing that checkpoint even when ordinary saves are deferred.
A retry may delete an unregistered folder only with that saved record, no `.git` entry anywhere below it, and no project checkout inside it.
Without the record, it keeps the folder and reports the cleanup error (#383).

The watcher reads GitHub every 15 seconds, with one query per project that lists the open PRs and the most recently closed or merged ones.
The query also reads the fields that show a change: the update time; the comment, review and thread counts; each thread's resolved state; and the head's checks.
A PR is read in full only when those fields moved since its last full read, when Velocity has not read it since starting, and every 30 minutes while it is open, which catches an edited inline review comment.
A PR with more than 100 review threads or head checks has the rest read on each pass, so every thread and check is compared.
A thread whose PR it does not follow, such as a draft claim, costs no request, and neither does a PR whose head is not the thread's branch until the list shows it changed.
The repository's identity is kept between passes and read again when the checkout's `origin` changes, which is checked locally each pass, and every 30 minutes.
Every `gh` call is counted by the code that made it, including the browser, new-project and repository-list calls, and the totals are written to `github-requests.log` in the app data folder each minute.

- **PR track** under the header: Task done → Proved → PR #n → CI → Review → Merged → Worktree deleted.
  A closed PR shows: … → Closed without merging → Worktree kept.
- **The transcript switches to the PR history:**
  - the coding work, collapsed to one line ("N tool calls · N files · +a −d · proved" with a Show work button)
  - PR opened, then the **proof card** (F17), CI results and reviewer claims
  - **review comments** as cards with file:line and open / ✓ resolved
  - the fix steps (edit, prove, push, reply) and "re-requested review"
  - approval, then a merged card
- **Agent behaviour:**
  - If the local checks or CI fail on its own change, fix and push.
  - Answer every review comment: either fix and push, or reply with the reason.
    Then resolve the thread and re-request review.
  - If the PR is closed without merging, report why in the thread.
- **Status line:** "watching PR #n · <stage> · answers new comments as they arrive", or "addressing N open review comment(s)".
- **Sidebar meta:** `#n` plus the stage label in the stage colour: checking / in review / addressing review / approved · merging.
  Once GitHub reports the merge, the label reads merged · deleting worktree until cleanup finishes and the thread settles as merged.

**Landing and rebases** (from Anthropic's Scion):

The rest of this section is the landing of a PR reviewed with the local checks (F7 "Checks before merge"): a sensitive PR, a PR that changes proofs, or a PR on a project with no GitHub checks.
A PR reviewed by reading skips it: Velocity merges its approved head once GitHub's checks pass, without updating its branch, and a merge never restarts another PR's checks.

The PR duty MCP server exposes `pr_duty_prepare_landing` and `pr_duty_checked`.
Prepare refuses a PR that is no longer open, so a closed PR's branch is never updated and its owner gets no `conflict` event; on an open PR it updates the branch through GitHub with an expected head and returns its exact head/base and required checks: full initially or on author/overlapping changes, build and lint for an unrelated moved base.
The reviewer fetches that head, regenerates artifacts, runs those checks and records success with checked.
Merge refuses a missing receipt or moved base.
Only an exact two-parent base update carries the reviewer's approval.
Updates and update failures post `updated`/`conflict` events.
Merge notices target claimed paths and edited regions within three lines, excluding generated artifacts and scoped registration claims.
At its next step the affected agent commits current edits and calls `airc_rebase`; Velocity updates only its registered checkout, on whichever branch the thread has checked out there (a thread may move to a new branch for its next piece), without rewriting history and posts `rebased` or `conflict`.

- **No global merge queue or lock.**
  A PR merges as soon as it is approved and its checks pass (F7), without waiting its turn behind other PRs.
- **Targeted rebases.**
  When a PR merges, Velocity compares the merged change's files with each open agent's claimed files and edited files (§3.2).
  - Only agents with an overlap rebase straight away; each gets a `rebased` event.
  - Every other agent rebases once, at landing time, instead of after every merge.
  - Generated files and registration lists (AGENTS.md "Shared files", F14) never count as an overlap.
    Generated files are regenerated, and registration lists take both sides' additions.
  - For edited files, an overlap means the changed regions touch or sit within a few lines of each other, not just that both changes are in the same file *(proposed)*.
- **PR duty updates branches.**
  PR duty agents are allowed, and encouraged, to update any open PR's branch with `master`.
  They don't wait for the authoring agent to rebase.
  - **When:** whenever a PR they are reviewing or about to merge is behind `master`, and again just before merging.
  - **How:** a merge of `master` into the PR branch (`gh pr update-branch`, or a merge commit pushed to the branch).
    PR duty never force-pushes and never rewrites the author's commits, so the author's history and open review threads stay intact.
    Each update posts an `updated` event naming the PR and the new head.
  - **Why:** waiting for each author to notice and rebase was the main source of landing delays.
    The author may be busy, rate-limited or paused, and every merge to `master` restarted the wait.
  - **The author** gets the `updated` event in its thread and pulls before it pushes again, so its next push builds on PR duty's update instead of being rejected.
- **Landing on a moved `master`.**
  PR duty updates the branch with `master` just before merging, then runs the checks on the merge result, not on the old PR head.
  - **`master` unchanged since the checks ran:** PR duty merges.
  - **No overlapping files:** if it applies cleanly and none of its files changed on `master` since the checks ran, PR duty rebuilds the merge result (incremental build and lint) and merges.
    It skips only the rest of the checks.
    A clean textual merge can still break the build: a field one PR adds to a struct breaks another PR's struct literal in a file the first PR never touched.
  - **Overlapping files, clean apply:** if files changed on `master` but the update applies cleanly, the checks run again first.
  - **Conflict PR duty can resolve:** when both sides are clear and the resolution keeps both changes as written (imports, lists, neighbouring hunks, lockfiles regenerated), PR duty resolves it in its review worktree, pushes the merge commit and re-runs the checks.
  - **Conflict that needs the author:** when resolving would mean choosing between two behaviours, the PR goes straight back to its own agent, with a `conflict` event naming the files.
    The agent resolves the conflict and pushes, then PR duty re-checks only the changed hunks and merges.
    A conflict is never routed through you.
- **Merge order** (`order` events, F7) is set only for PRs that overlap each other.
  Unrelated PRs never wait on each other.

**Accept when:**
- [ ] A merge rebases only the open agents whose claimed or edited files it touched.
- [ ] A PR that falls behind `master` is updated by PR duty without waiting for its author, and the author's commits are never rewritten.
- [ ] A conflict at landing reaches the owning agent within one event cycle and is resolved without user input.
- [ ] A comment from a PR duty agent reaches the authoring thread automatically and is acted on without user input.
- [ ] The thread is not finished until `pr://merged`.
