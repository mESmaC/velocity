#### PR duty · Approval record · A changes event newer than the approval blocks the merge

<!-- split-spec: {"order":1041,"kind":"markdown","indentation":2} -->
- A `changes` event newer than the approval blocks the merge.
  PR duty mirrors each record on GitHub as a comment that starts "Approved at `<sha7>`" and gives a short reason for the approval, so people reading the PR see it.
  Velocity then merges on the approver's behalf, or the reviewer merges itself after local checks.
