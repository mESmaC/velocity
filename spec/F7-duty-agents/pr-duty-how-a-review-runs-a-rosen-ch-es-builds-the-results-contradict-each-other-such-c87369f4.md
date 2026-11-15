#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests · The results contradict each other, such as a test that fails only sometimes or a failure every commit has

<!-- split-spec: {"order":1028,"kind":"markdown","indentation":6} -->
- The results contradict each other, such as a test that fails only sometimes or a failure every commit has: nothing is reverted, the feed says so, and the batch is tried again after the next land.
  The same holds when Rosen refuses a result, such as a revert that no longer merges.
