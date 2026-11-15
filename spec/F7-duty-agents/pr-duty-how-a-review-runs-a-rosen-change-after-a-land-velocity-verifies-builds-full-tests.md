#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests · Full tests

<!-- split-spec: {"order":1023,"kind":"markdown","indentation":6} -->
- **Full tests:** the project's `rosen.toml` post-land commands run on the newest landed commit no full run has covered, in `post-land-checks`.
  Both folders keep build output and rewrite only changed sources, as `pre-land-checks` does.
  Full tests run at below-normal priority in the queue's `verify` class: a run starts when no foreground build is running or waiting, or after two minutes, and once started it never holds one of them back.
