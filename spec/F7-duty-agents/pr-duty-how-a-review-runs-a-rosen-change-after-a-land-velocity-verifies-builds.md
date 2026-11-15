#### PR duty · How a review runs · A Rosen change · After a land, Velocity verifies builds and runs the full tests

<!-- split-spec: {"order":1020,"kind":"markdown","indentation":4} -->
- **After a land, Velocity verifies builds and runs the full tests** (Rosen R12, #380).
  In `after-land` mode these are independent passes that run even without active PR duty agents or while PR duty is paused.
  In `before-land` mode the full-test pass retains today's PR duty schedule and revert policy; builds remain before landing, and `recheck = "merge"` also runs build/lint after landing, independently of PR duty pause or staffing (#391).

<!-- split-spec links -->

Rule files:

- [Checker and post-land messages remain visible without a PR duty role (#392)](pr-duty-how-a-review-runs-a-rosen-ch-ies-builds-checker-and-post-land-messages-remain-0d35a4d4.md)
- [Fast builds in after-land mode](pr-duty-how-a-review-runs-a-rosen-ch-y-verifies-builds-fast-builds-in-after-land-mode-b5d1a96c.md)
- [Full tests](pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-full-tests.md)
- [Every command passed](pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-every-command-passed.md)
- [A command failed](pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-a-command-failed.md)
- [build](pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-build.md)
- [test](pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds-test.md)
- [The results contradict each other, such as a test that fails only sometimes or a failure every commit has](pr-duty-how-a-review-runs-a-rosen-ch-es-builds-the-results-contradict-each-other-such-c87369f4.md)
- [The checks could not run at all](pr-duty-how-a-review-runs-a-rosen-ch-city-verifies-builds-the-checks-could-not-run-at-2238b38e.md)
- [A project with no post-land commands has nothing to fail](pr-duty-how-a-review-runs-a-rosen-ch-city-verifies-builds-a-project-with-no-post-land-0aca7f21.md)
