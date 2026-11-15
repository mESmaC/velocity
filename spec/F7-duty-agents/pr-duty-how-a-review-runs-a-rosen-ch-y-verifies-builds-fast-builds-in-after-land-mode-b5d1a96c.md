#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests · Fast builds in after-land mode

<!-- split-spec: {"order":1022,"kind":"markdown","indentation":6} -->
- **Fast builds in `after-land` mode:** Rosen offers the newest base commit not covered by its build prefix, batching lands that arrive together.
  Velocity brings `post-land-builds` to that commit and runs its `rosen.toml` pre-land commands (build and lint, targeting 1–3 minutes).
  Velocity preserves Rosen's offered check kind, commands and priority: `landing` runs in the foreground queue class and `verify` runs below normal priority in the background class.
  Merge-mode build jobs use `landing`, ahead of full tests.
  The independent build worker handles build jobs that Rosen offers before tests through `post_land_next`, so the test worker never runs them as tests (#391).
  Passed and failed build feed lines include the fast script's elapsed time and cache delta from `target/.velocity-checks/feed.txt` when present; the optional report is bounded and never affects the verdict.
