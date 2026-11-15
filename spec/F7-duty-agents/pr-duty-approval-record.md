#### PR duty · Approval record

<!-- split-spec: {"order":1037,"kind":"markdown","indentation":0} -->
- **Approval record** (decided, Q28).
  Velocity records each approval itself, as an `approved` AIRC event naming the PR, the reviewer's nick and the head SHA it approved.
  That event is the approval Rule 4 requires.

<!-- split-spec links -->

Rule files:

- [GitHub's review state is advisory, like CI](pr-duty-approval-record-github-s-review-state-is-advisory.md)
- [PR duty merges only the head it approved (gh pr merge --match-head-commit <sha>), so a commit pushed after approval needs a new review](pr-duty-approval-record-pr-duty-merges-only-the-head.md)
- [A branch update PR duty made itself with no conflict resolution moves the approval to the new head (it is not a new commit to review, see Rules below)](pr-duty-approval-record-a-branch-update-pr-duty-made.md)
- [A changes event newer than the approval blocks the merge](pr-duty-approval-record-a-changes-event-newer-than-the.md)
- [On a repository whose branch protection requires a GitHub approval, the merge fails and Velocity posts a failed event](pr-duty-approval-record-on-a-repository-whose-branch-protection.md)
