#### Review swarm on after-land Rosen projects (#382) · Completion is durable

<!-- split-spec: {"order":1089,"kind":"markdown","indentation":0} -->
- Completion is durable.
  Velocity marks the target `reviewed` or `issues filed` only after a completion report and after any named proof or fix-forward work lands.
  An ended or failed session alone never marks coverage.
  Interrupted workspaces and pending acknowledgements resume after restart; unfinished edits are kept.
  A reverted and relanded target is a new review incarnation.
- When a land has a source issue, the reviewer checks that issue's acceptance criteria against the exact landed SHA and records `source_issue_satisfied` with the completion report.
  Velocity applies issue closure on behalf of the review swarm after a positive review and once the mirror carries the commit (F9, #441).
  A negative source-issue verdict is recorded as soon as it is submitted, keeping the issue open even if the reviewer's fix needs more time to land.
- A dispatched issue reported as `no-change` requests an independent review when the swarm is idle (F9, #469).
  The reviewer checks the issue against the current base and uses `review_swarm_no_change` to record verification and close it or to post retry instructions and release it back to dispatch.
