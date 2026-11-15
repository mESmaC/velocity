#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests · Checker and post-land messages remain visible without a PR duty role (#392)

<!-- split-spec: {"order":1021,"kind":"markdown","indentation":6} -->
- Checker and post-land messages remain visible without a PR duty role (#392): the first update creates a saved, zero-agent PR duty feed entry under On duty.
  It starts no agents and retains the latest 200 events across restarts.
  Existing roles keep their agents, rules and pause state; projects or other duty roles removed during a run are not recreated.
