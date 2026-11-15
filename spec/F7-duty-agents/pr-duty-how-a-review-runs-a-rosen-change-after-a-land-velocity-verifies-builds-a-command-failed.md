#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests · A command failed

<!-- split-spec: {"order":1025,"kind":"markdown","indentation":6} -->
- A command failed: Velocity gives Rosen the command, its exit status and the end of its output, then tests each commit Rosen names with the failed batch's commands and priority, so a build bisection runs build/lint rather than full tests.
  Rosen halves the commits since the last green one until a single change is left, and wants that change to fail a second time and the commit before it to pass.
