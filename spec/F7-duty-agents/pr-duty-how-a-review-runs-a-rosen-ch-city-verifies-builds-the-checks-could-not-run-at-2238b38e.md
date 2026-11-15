#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests · The checks could not run at all

<!-- split-spec: {"order":1029,"kind":"markdown","indentation":6} -->
- The checks could not run at all: the feed says why once, and the next tick tries again.
  Rosen keeps a bisection across restarts, so a pass picks it up where it stopped.
