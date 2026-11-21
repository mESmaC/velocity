#### Rosen · Issues carried by Rosen changes (#386)

<!-- split-spec: {"order":578,"kind":"markdown","indentation":0} -->
- **Issues carried by Rosen changes (#386).**
  An issue dispatch keeps its repository and number on the thread and records a resolved source-issue note on its Rosen change, anchored to SPEC.md; the agent is told to keep the `(#N)` suffix in its named snapshot.
  Each linked Task List entry has its own resolved note carrying the project and task number, and a stacked replacement records the same source items before retiring its predecessor.
  Velocity posts a comment naming the Rosen change and landed commit on every land.
  Once the mirror carries that exact commit, Velocity closes on behalf of a reviewer who confirmed the issue's acceptance criteria.
  Velocity can close after the 30-minute R12 window in F7, provided the land has no review claim or objection.
  A revert cancels a pending close and reopens the issue with the culprit SHA and output.
  These effects belong to the Rosen event follower, not the checker; they survive restarts, retry failed GitHub calls and mirror lag every half minute, and reuse their comments when replayed.
  Follow-up markers deduplicate by action kind and commit without the machine-local event sequence, and closes are skipped when the issue is already closed with a close comment covering the same commit (#550).
  Swarms with Close issues switched off leave their issues open.
