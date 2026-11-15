#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests · test

<!-- split-spec: {"order":1027,"kind":"markdown","indentation":6} -->
- A confirmed **test** failure in `after-land` mode is retained by Rosen as a `post-land-failure`, with its culprit, and never reverted.
  Velocity files an issue on the project's GitHub repository labelled `post-land-failure`, containing the command, exit status, output, culprit change and commit, and its saved thread.
  A later occurrence for the same culprit and command/status comments on the existing open issue, including when diagnostic timings or paths differ.
  Stable occurrence markers prevent a retry after a crash from posting twice; a durable outbox retries GitHub outages independently of verification and landing.
  In `before-land` mode a confirmed test failure still reverts and reopens its culprit as before.
  Full tests gate releases (#381), not lands in `after-land` mode.
