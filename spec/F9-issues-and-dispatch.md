<!-- split-spec: {"order":1148,"kind":"markdown","indentation":0} -->
### F9 · Issues and dispatch

**Rosen issues in Work (decided, #440).** Open GitHub issues share the Work panel with Task List entries (F8).
The row shows its GitHub issue badge, number, title, labels and scout attribution alongside the live claim and Rosen state.
Thread and change links appear as each becomes available, and the issue itself remains directly accessible on GitHub.
A tracked issue remains in the Work history after GitHub closes it.
Until GitHub catches up, local completion keeps the issue out of dispatch even if GitHub still lists it as open.

**Questions and details.** `spec-question` rows stay in a separate group at the top, ordered by how many pieces they block.
Their expanded rows show the recommended answer from a `Recommended answer:` or `Recommendation:` paragraph and the blocked count from the `Blocks:` bullets.
Expanding an ordinary issue shows its opening paragraph and the latest agent comments from #441, with attribution and a link to the GitHub discussion.
Expansion starts collapsed and is saved per project and repository-qualified issue, including across restarts.
Click, Enter, Space and the arrow keys retain their row expansion behaviour.
Expanded issue rows load and refresh their latest agent comments independently of Rosen state.
A slow or failed comment fetch leaves work and dispatch available, and the row can retry its discussion.

**Issue dependencies (#472).** Issue rows show `Blocked by #n` for open GitHub blockers.
A Work row shows `Unblocks n` when n open issues wait on it, directly or through other open issues, in any project's repository (#510).
Continuous dispatch starts the highest count first (F8), **Next work** lists it as `unblocks n`, and the owner overrides the order by dispatching a row by hand.
When a decision sets an order between issues, record it as a GitHub issue dependency.
The review swarm can record dependencies between project issues with `scout_add_blocker { issue, blocker }`.

**Dependencies when filing (#511).** An issue that waits on other issues is held by GitHub `blocked_by` links alone, and never by a hold label.
Every tool that files or updates an issue takes the blockers as `blocked_by`, a list of `#n` for the same repository or `owner/name#n` for another.
`scout_file`, `issue_comment`, `issue_needs_owner`, `parked_review_resolve` and `review_swarm_no_change` link them on the issue they write, and `scout_add_blocker` takes `blocker_repository` for a blocker in another repository.
A reported discovery carries its blockers to the filed issue (F14).
A link that already exists is kept, a blocker that does not exist is refused before `scout_file` runs the reproduction, and the comment names the linked issues.
The tools refuse `needs-owner` when its reason only waits on other issues: it names issues beside waiting words and names no hardware, upstream release or owner decision.
The refusal lists those issues and says to pass them as `blocked_by` instead.
Dispatch starts a linked issue when its blockers close, with no label change.
An empty dispatch turn on an issue with open blockers posts its **Ended with no change** comment without the `no-change` label.
Each issue read also converts any open issue whose body still carries a `Held for dependencies:` line that names issues.
The conversion links those issues, removes `needs-owner` and rewrites the line to name the links, in that order, so a failure leaves the issue held and each note is converted once.
At most five notes are converted per read, and a note that cannot be converted is retried hourly.

**Dispatch and Accept.** The hover action, `I` and the palette retain single-issue dispatch.
Dispatch claims the repository-qualified issue atomically, opens the owning thread if one already exists, retains the issue title and launches in its own Rosen workspace.
Issue dispatch, continuous dispatch, swarms and dispatch workers pause new agent starts while the machine is below Rosen's commit reserve or its available physical memory is at most 4 GiB, capped at one fifth of installed memory (#481).
The opening prompt fits in 4,000 characters and links to the full GitHub description and evidence when the issue body is excerpted.
Accept starts work scoped to recording the recommended one-line proposed answer in the spec through the ordinary Rosen lifecycle.
The Work panel uses change and land vocabulary for Rosen projects.

**Overlapping issues (#471).** Continuous dispatch holds an issue while another open issue is being worked only when that issue's open Rosen change has edited a code file the waiting issue names (#525).
Predicted paths, claimed paths, similar titles, documents, spec and design files and root `ops/` scripts never hold an issue, because Rosen merges and reports conflicts and a speculative hold costs more than a rare conflict.
The generated artifacts and registration lists in the base's `rosen.toml` never hold an issue either, because Rosen regenerates the first and keeps both sides' additions in the second (#505).
A path named by more than a third of the ready backlog, and by more than three of its issues, holds nothing, because a file everyone touches says nothing about who conflicts (#505).
The ready backlog is every open issue that is being worked, held or free for dispatch.
The same paths are left out when a land is checked against a waiting issue's code.
The check reads each open change's edited files from Rosen at most every 30 seconds and is repeated before starting each issue.
After the blocking change lands, Velocity rechecks the current issue text against the source issue and the landed paths.
A land that shares code with the issue or may have fixed it parks the issue under `no-change` as **Check against an overlapping land** for a bounded review (#506).
When the blocker ends without a land, the waiting issue returns to ordinary dispatch.
When the land changed none of the issue's predicted code paths and the reports describe different problems, Velocity posts the land's files on the issue as evidence and returns it to dispatch in the same pass without a review (#506).
The overlap check and its **Check against an overlapping land** review apply only to issues with no landed change of their own (#504).
An open issue is held on overlap once (#525).
When its hold ends, through a review, a land without evidence or a blocker whose change no longer edits its files, dispatch starts the issue and never holds it on overlap again.
An issue whose GitHub record shows an overlap review that returned it to dispatch is never held.
A held issue's Work card shows the hold, its reason and the blocker's overlapping files.

**Issue follow-through.** Velocity's Rosen follower supplies the land and verified-build evidence and posts the land update (F5, Issues carried by Rosen changes).
Under the #441 decision, the review swarm closes the issue after confirming that the landed change meets the issue's acceptance criteria.
Post-land verification handles closure after 30 minutes from verified post-land build success if no reviewer has covered that land; an objection rules out the fallback.
Closure waits for the GitHub mirror to contain the landed commit and honours **Close issues**.
A revert cancels pending closure and reopens the issue with the culprit output or reason.
Discussion and closure happen asynchronously alongside the Work projection and landing.

**Issue comments (#441).** AIRC's `issue_comment { issue, body }` posts on any project issue the calling agent currently holds.
The comment includes the agent's nick and a link to its saved Velocity thread when it has one.
Agents comment when starting, when a finding changes the plan and when blocked.
On Rosen, Velocity posts the land comment with the change ID and commit.
The author can call `issue_ready_to_close { issue, body }` to post completion evidence and add the `ready-to-close` label.
The author leaves the issue open for the review swarm or post-land verification (F7).
An agent whose issue cannot proceed without hardware, an upstream release or an owner decision calls `issue_needs_owner { issue, body }`, which adds the `needs-owner` label with the reason (#506).
An agent whose issue waits on other issues calls `issue_comment { issue, body, blocked_by }` and ends its turn (#511).

**Empty dispatch turns (#469).** Velocity adds the `no-change` label and posts an **Ended with no change** comment with the agent's last message and thread link when an automatically dispatched issue thread ends with an empty Rosen change (F8).
Velocity retries an undelivered report across restarts and uses the Rosen change ID to avoid duplicate comments.
An issue already labelled `needs-owner` gets the comment without the `no-change` label (#506).

**Parked reviews (#506).** An open issue labelled `no-change` leaves review without a staffed review swarm.
A parked **Check against an overlapping land** whose land shares no code with the issue returns to dispatch within one dispatch pass, with the land's files as evidence.
Every other report gets one bounded review run: a light, read-only session in a fresh workspace at the current base, one per project at a time and stopped after 20 minutes.
The session uses `parked_review_read` and `parked_review_resolve` to close the issue with verification evidence, return it to dispatch with retry instructions, or move it to `needs-owner` with the reason.
Its Rosen change is abandoned with anything the session wrote and is never landed.
A report still pending two hours after it was posted returns the issue to dispatch with a note.
Work that needs hardware, an upstream release or an owner decision takes `needs-owner` with the reason and never stays `no-change`.
A person or review swarm agent can also close a parked issue after verifying the current base, post retry instructions and remove the label, or move it to `needs-owner`.
A paused dispatch starts no bounded review; the evidence check and the two-hour limit still apply while the project has dispatch slots.

**Git projects.** The existing Issues panel and dispatch apply while git project mode is supported.

- **The Issues panel** lists open GitHub issues: number, title, label and the scout that filed it.
- **Questions** *(proposed)*: issues labelled `spec-question` are open SPEC.md §8 questions an agent ran into.
  They show as their own group at the top of the panel, sorted by how many pieces each one blocks.
  Each row shows the filer's recommended answer when expanded.
- **Collapsed rows**: every row starts collapsed to its title line and label line, so a long list stays scrollable.
  Clicking a row (or Enter, Space, →/←) expands it to show the recommendation and blocked count for a question, or the opening paragraph of the issue body, with an Open button that opens the issue (or its thread, once dispatched).
  Which rows are expanded is per project and survives refreshes.
  - **Accept** opens a one-line SPEC.md PR that records the answer as *(proposed)*.
    The named change lands through Rosen (F7), and the blocked pieces become claimable.
  - Agents never wait on a question: they file it and claim other work (Rule 1).
  - **Why:** unanswered questions, not code, were what kept most of a milestone unclaimable.
- **Dispatch** (hover button, `I`, or the palette): starts one agent on the issue, in its own workspace, with the normal lifecycle.
  The issue stays listed with its agent's live state until the PR merges, which closes it.
  - `I` opens the palette filtered to issues.
    Dispatched threads keep the issue title and use `vel/issue-<n>` (with a unique suffix when needed), without calling the title model.
    Their saved issue identity includes the repository; repeat dispatch opens the owning thread.
    Deleting that thread frees the issue after Undo expires (F12).
  - A question's recommended answer is read from a `Recommended answer:` or `Recommendation:` paragraph; its `Blocks:` bullet list supplies the blocked-item count.
    Questions without a recorded recommendation stay readable on GitHub.
    Accept starts the normal agent lifecycle with a prompt limited to the one-line proposed SPEC answer and its PR.
