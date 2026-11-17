<!-- split-spec: {"order":1148,"kind":"markdown","indentation":0} -->
### F9 · Issues and dispatch

- **The Issues panel** lists open GitHub issues: number, title, label and the scout that filed it.
- **Questions** *(proposed)*: issues labelled `spec-question` are open SPEC.md §8 questions an agent ran into.
  They show as their own group at the top of the panel, sorted by how many pieces each one blocks.
  Each row shows the filer's recommended answer when expanded.
- **Collapsed rows**: every row starts collapsed to its title line and label line, so a long list stays scrollable.
  Clicking a row (or Enter, Space, →/←) expands it to show the recommendation and blocked count for a question, or the opening paragraph of the issue body, with an Open button that opens the issue (or its thread, once dispatched).
  Which rows are expanded is per project and survives refreshes.
  - **Accept** opens a one-line SPEC.md PR that records the answer as *(proposed)*.
    PR duty lands it as Trivial (F7), and the blocked pieces become claimable.
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
