#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests · Full tests

<!-- split-spec: {"order":1023,"kind":"markdown","indentation":6} -->
- **Full tests:** the project's `rosen.toml` post-land commands run in `post-land-checks`, using the queue's `verify` class.
  In `after-land` mode the same pass runs the configured build, lint and test commands as one integration suite.
  In `before-land` mode the full-test pass follows the existing PR duty schedule.
