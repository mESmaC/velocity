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
Expansion starts collapsed and is saved per project.
Click, Enter, Space and the arrow keys retain their row expansion behaviour.
Issue discussion loads independently of Rosen state; a slow or failed comment fetch leaves work and dispatch available.

**Issue dependencies (#472).** Issue rows show `Blocked by #n` for open GitHub blockers.
When a decision sets an order between issues, record it as a GitHub issue dependency.
The review swarm can record dependencies between project issues with `scout_add_blocker { issue, blocker }`.

**Dispatch and Accept.** The hover action, `I` and the palette retain single-issue dispatch.
Dispatch claims the repository-qualified issue atomically, opens the owning thread if one already exists, retains the issue title and launches in its own Rosen workspace.
Issue dispatch, continuous dispatch, swarms and dispatch workers pause new agent starts while the machine is below Rosen's commit reserve or its available physical memory is at most 4 GiB, capped at one fifth of installed memory (#481).
The opening prompt fits in 4,000 characters and links to the full GitHub description and evidence when the issue body is excerpted.
Accept starts work scoped to recording the recommended one-line proposed answer in the spec through the ordinary Rosen lifecycle.
The Work panel uses change and land vocabulary for Rosen projects.

**Overlapping issues (#471).** Continuous dispatch holds an issue while another open issue is being worked if the reports describe the same problem or their code graph references overlap.
The check includes the active issue's claimed paths and is repeated before starting each issue.
After the blocking change lands, Velocity rechecks the current issue text against the source issue and the landed paths.
A plausible fix goes to the independent review swarm to verify against the current base and close, or return with retry instructions.
When the blocker ends without a land or its land shows no evidence for the waiting issue, the waiting issue returns to ordinary dispatch.

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

**Empty dispatch turns (#469).** Velocity adds the `no-change` label and posts an **Ended with no change** comment with the agent's last message and thread link when an automatically dispatched issue thread ends with an empty Rosen change (F8).
Velocity retries an undelivered report across restarts and uses the Rosen change ID to avoid duplicate comments.
A person or review swarm agent can close the issue after independently verifying the current base meets its acceptance criteria, or post concrete retry instructions and remove the label.
If the blocker still prevents a productive retry, the issue stays labelled.

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
