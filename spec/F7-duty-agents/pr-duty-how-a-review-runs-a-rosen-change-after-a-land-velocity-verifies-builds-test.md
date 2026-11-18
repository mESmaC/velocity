#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests · test

<!-- split-spec: {"order":1027,"kind":"markdown","indentation":6} -->
- A confirmed test failure in the after-land integration suite uses the same bisection and fix-forward path as a compile or lint failure.
  A confirmed test failure in `before-land` mode still reverts and reopens its culprit.
  The release gate requires a green integration build on the release commit.
