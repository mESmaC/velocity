<!-- split-spec: {"order":788,"kind":"markdown","indentation":0} -->
### F6 · Change follow-through

Velocity follows the selected Rosen project through its event stream (R9).
A named snapshot moves the owning thread into review, a land settles it and releases its claims, and a revert reopens the change with the failed check output (F5, F7).
The rail reads Rosen directly and reconnects when the stream drops.
It takes its initial snapshot on selection and refreshes when Rosen publishes events or changes local state that has no R9 event in the pinned daemon.

The Rosen status pane has seven collapsible sections.
The window saves their order and collapsed state.

| Section | Live information |
|---|---|
| Server | Running state, version, connected R14 machines, and the local run queue holders and waiter count |
| Base | Base tip, last green commit, bisect progress, and current culprit or revert |
| Landing | Lands in the last hour, median time from naming to land for that hour, and post-land build and full-test backlog |
| Claims and conflicts | Paths, claim holders, overlaps and open stored conflicts |
| Warm pool | Ready workspace count and target |
| Mirror | Mirror state and last pushed commit, with the Collapse downstream forwarder state supplied by #427 when available |
| Changes | Open changes and recent outcomes with the owning thread, line counts, claims, overlaps, conflicts and stacked parent |

An owned change row opens its thread.
Rows show editing, named, landing, landed, green, reverted or abandoned.
A landed row includes the resulting base commit.
A reverted row shows the saved culprit output.

#### Rosen thread view

The header shows the Rosen change ID and its workspace path.
Name change records a named snapshot, Rebase brings the workspace onto the current base, and Abandon closes the change and lets Rosen remove the workspace.
The change track shows Editing → Named → Landing → Landed → Green.
The sidebar uses the same stage and its colour.
The transcript shows the files with stored conflicts and offers Resolve with agent, which queues the handoff for the owning agent's next turn boundary.
After a land the transcript groups the coding work behind Show work and shows the landed commit, its Rosen change and the recorded post-land results.
Historical lands retain any receipt and approver trailers Rosen recorded.
A revert shows Reverted → Reopened with the inverse commit and failing output, restores the transcript and composer, and resumes the owning agent through the F5 recovery path.
Land attempts and their evidence stay in the thread after a reland or restart.

**Accept when:**

- [ ] Naming, landing, reverting and abandoning update the row within a second without a GitHub request from the pane.
- [ ] Section order and collapsed state survive reopening Velocity.
- [ ] A Rosen thread shows naming, landing, check results and any reopen in its own view.

#### Legacy Git thread follow-through

When the task is done, the agent opens a PR.
The PR description links the Task List item or says `Fixes #n`.
The thread moves to **PR open · watching**.

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

**Targeted rebases for legacy Git threads.**
When an observed PR merges, overlapping claims and edits determine which threads receive a rebase notice.
Generated files and registration lists do not count as overlaps.
The affected agent commits current edits before `airc_rebase` updates its registered checkout without rewriting history.

**Accept when:**
- [ ] A merge rebases only the open agents whose claimed or edited files it touched.
- [ ] A conflict at landing reaches the owning agent within one event cycle and is resolved without user input.
- [ ] The thread is not finished until `pr://merged`.
