#### PR duty · How a review runs · A Rosen change

<!-- split-spec: {"order":1005,"kind":"markdown","indentation":2} -->
- **A Rosen change** (F5) follows the `review` setting in the base's `rosen.toml`.
  In `after-land` mode the checker asks Rosen to land a named change without running any pre-land command or submitting a receipt.
  A conflict goes back to its author; a receipt refusal never starts a build in this mode.
  Review and full tests do not block another land.
  In `before-land` mode it is claimed like a PR once its thread names its work, after the project's PRs, and Rosen requires an approval at the change's head and a receipt for its merge result.
  The reading and local-check review paths below describe that mode.

<!-- split-spec links -->

Rule files:

- [Reviewed by reading](pr-duty-how-a-review-runs-a-rosen-change-reviewed-by-reading.md)
- [Velocity checks and lands the approved head](pr-duty-how-a-review-runs-a-rosen-change-velocity-checks-and-lands-the-approved.md)
- [After a land, Velocity verifies builds and runs the full tests](pr-duty-how-a-review-runs-a-rosen-change-after-a-land-velocity-verifies-builds.md)
- [Reviewed with the local checks](pr-duty-how-a-review-runs-a-rosen-change-reviewed-with-the-local-checks.md)
- [The review runs in the project's review-change-<thread>-<nickname> folder](pr-duty-how-a-review-runs-a-rosen-change-the-review-runs-in-the-project.md)
- [Checks in a plain merge tree (#394, #378, #388)](pr-duty-how-a-review-runs-a-rosen-change-checks-in-a-plain-merge-tree.md)
- [The agent works only through PR duty's Rosen MCP tools, served by Velocity's own executable](pr-duty-how-a-review-runs-a-rosen-change-the-agent-works-only-through-pr.md)
- [Requested changes go to the author's thread as a message and put it back in review, stage fixing; its line comments open Rosen review threads](pr-duty-how-a-review-runs-a-rosen-change-requested-changes-go-to-the-author.md)
- [A land Rosen refuses because the base moved needs a fresh merge result, checks and receipt; any other refusal ends the review with a feed line](pr-duty-how-a-review-runs-a-rosen-change-a-land-rosen-refuses-because-the.md)
