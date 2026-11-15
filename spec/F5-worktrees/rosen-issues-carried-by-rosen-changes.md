#### Rosen · Issues carried by Rosen changes (#386)

<!-- split-spec: {"order":578,"kind":"markdown","indentation":0} -->
- **Issues carried by Rosen changes (#386).**
  An issue dispatch keeps its repository and number on the thread and records a resolved source-issue note on its Rosen change, anchored to SPEC.md; the agent is told to keep the `(#N)` suffix in its named snapshot.
  After Rosen reports the land and a successful post-land build covering that land, Velocity closes the issue with a comment naming the landed commit's full SHA, that it landed through Rosen, that the build passed after the land, and that the GitHub mirror carries it.
  Closure waits until GitHub's mirror branch contains that commit.
  Test green events never complete an issue.
  The follower reads Rosen's separate durable build-success prefix, requires its build check set to contain commands, and verifies that the landed SHA is an ancestor of the successful build's commit.
  Without that protocol or a covering build success, closure remains pending.
  A revert cancels a pending close and reopens the issue with the culprit SHA and the revert's reason.
  These effects belong to the Rosen event follower, not the checker; they survive restarts, retry failed GitHub calls and mirror lag every half minute, and reuse their comments when replayed.
  Swarms with Close issues switched off leave their issues open.
