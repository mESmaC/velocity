#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests · Batched integration in after-land mode

<!-- split-spec: {"order":1022,"kind":"markdown","indentation":6} -->
- **Batched integration in `after-land` mode.**
  Velocity takes the newest eligible base commit, stages it in `post-land-checks`, and reuses that folder's build output.
  It takes a machine-wide integration slot and a Rosen `verify` turn before running the commands.
  The completed batch bounds both the ten-land count and the two-hour window for the next busy period.
  A green result records the exact commit and covers earlier lands for issue follow-through.
  The GitHub mirror receives the exact commit's `velocity/integration` status from a durable outbox.
