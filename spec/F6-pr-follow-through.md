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
| Base | Base tip, last green commit, GitHub CI on the mirrored base with its failing jobs (F5 Base CI, #507), bisect progress, current culprit or revert, and Cut release |
| Landing | Lands in the last hour, median time from naming to land for that hour, and post-land build and full-test backlog |
| Claims and conflicts | Paths, claim holders, overlaps and open stored conflicts |
| Warm pool | Ready workspace count and target |
| Mirror | Mirror state and last pushed commit; for a Collapse mirror, Collapse's last land with its agent and the downstream forwarder state (in sync, catching up with lands behind, retrying with the next attempt, stopped with git's message, or no downstream), and the state feed to Collapse (when it last sent, failing with Rosen's error, or commits only with Send Collapse its live state, #517) |
| Changes | Open changes and recent outcomes with the owning thread, line counts, claims, overlaps, conflicts and stacked parent |

An owned change row opens its thread.
Rows show editing, named, landing, landed, green, reverted or abandoned.
A landed row includes the resulting base commit.
A reverted row shows the saved culprit output.

Cut release takes a version and, optionally, a landed commit, and releases Rosen's newest green commit when the commit is left empty (#431, #521, #593).
When the project mirrors to Collapse, Cut release builds on this machine via `scripts/release-local.ps1 -Sign`, signs every download with the updater key held in Windows Credential Manager (`velocity:updater-signing-key`), publishes the release, signed assets and standalone Linux `velocity-graph` archive to Collapse, and rewrites `latest.json` on Collapse's rolling `updater` release (#593).
When a requested commit has no green integration build yet, Velocity starts an immediate integration build covering it and cuts the release automatically once it passes (#521, #593).
For GitHub-only projects without a Collapse mirror, Cut release waits up to three minutes for GitHub's default branch to carry the green commit and its `velocity/integration` status, then dispatches `release.yml` with the tag, the commit and the green commit (#431, #521).

#### Rosen thread view

The header shows the Rosen change ID and its workspace path.
Name change records a named snapshot, Rebase brings the workspace onto the current base, and Abandon closes the change and lets Rosen remove the workspace.
The change track shows Editing → Named → Landing → Landed → Green.
The sidebar uses the same stage and its colour.
When Rosen refuses a land for regeneration or conflicts, Velocity returns the thread to Working and wakes its agent in the same pass with the affected paths and repair instructions.
For regeneration, the agent rebases the workspace, runs the generators for the listed files on the merged inputs and names the repaired change for another land attempt.
Velocity retries any other refusal once on a later pass and, if Rosen still refuses, returns the thread to Working with Rosen's reason and advice.
The thread card shows the refusal until the author names the change again.
The transcript shows the files with stored conflicts and offers Resolve with agent.
Clicking Resolve with agent delivers immediately when the thread is idle or its session has ended, resuming the session and recreating a missing workspace from the change when possible (#522).
When delivery must wait or a resolution is already pending, the Stored conflicts card shows that resolution is queued and where it is waiting until the agent receives the prompt in the transcript.
If a missing workspace cannot be recreated, the card explains why the change cannot be resolved here and offers Abandon.
Background Rosen follow-up delivery failures appear as a notice on the thread.
After a land the transcript groups the coding work behind Show work and shows the landed commit, its Rosen change and the recorded post-land results.
Historical lands retain any receipt and approver trailers Rosen recorded.
A revert shows Reverted → Reopened with the inverse commit and failing output, restores the transcript and composer, and resumes the owning agent through the F5 recovery path.
Land attempts and their evidence stay in the thread after a reland or restart.

**Accept when:**

- [ ] Naming, landing, reverting and abandoning update the row within a second without a GitHub request from the pane.
- [ ] Section order and collapsed state survive reopening Velocity.
- [ ] A Rosen thread shows naming, landing, check results and any reopen in its own view.

#### Legacy Git threads

Velocity no longer follows GitHub pull requests (#426).
A thread on a project that uses Git keeps its transcript, branch and workspace until you delete it (F12), and Velocity never reads, closes or opens its PR.
Threads saved with an earlier PR record load without it, and their past PR events stay in the AIRC log.
GitHub remains for issues, scout filing, post-land failure issues and the repository list in Open from GitHub (F2, F7, F9).
