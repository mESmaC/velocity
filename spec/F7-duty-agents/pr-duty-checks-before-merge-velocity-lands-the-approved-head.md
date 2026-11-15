#### PR duty · Checks before merge · Velocity lands the approved head

<!-- split-spec: {"order":974,"kind":"markdown","indentation":2} -->
- **Velocity lands the approved head.**
  Every 30 seconds Velocity reads each approved head no agent holds and merges it when all of these hold: the PR is open, ready and still at the approved head; it merges cleanly; every GitHub check on the head has completed as success, neutral or skipped; and no review thread is open.
  No agent takes part, and the branch is not updated first.

<!-- split-spec links -->

Rule files:

- [Checks still running](pr-duty-checks-before-merge-velocity-lands-the-approved-head-checks-still-running.md)
- [A failed check](pr-duty-checks-before-merge-velocity-lands-the-approved-head-a-failed-check.md)
- [A conflict with the base branch](pr-duty-checks-before-merge-velocity-lands-the-approved-head-a-conflict-with-the-base-branch.md)
- [An open review thread](pr-duty-checks-before-merge-velocity-lands-the-approved-head-an-open-review-thread.md)
- [The merge is the approver's](pr-duty-checks-before-merge-velocity-lands-the-approved-head-the-merge-is-the-approver-s.md)
