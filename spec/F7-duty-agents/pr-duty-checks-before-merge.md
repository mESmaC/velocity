#### PR duty · Checks before merge

<!-- split-spec: {"order":972,"kind":"markdown","indentation":0} -->
- **Checks before merge.**
  These gates apply to git PRs and Rosen projects with `review = "before-land"` (the default).
  A Rosen project with `review = "after-land"` has no build, receipt or approval gate at land time: its named changes land on a clean merge, and Velocity verifies builds and tests afterwards as described below.
  Authors still build and lint their work through `rosen run` before naming it.
  A reviewer reads a PR; it does not rebuild it.
  GitHub's checks build and test the head, and Velocity merges it once they pass.

<!-- split-spec links -->

Rule files:

- [Review by reading](pr-duty-checks-before-merge-review-by-reading.md)
- [Velocity lands the approved head](pr-duty-checks-before-merge-velocity-lands-the-approved-head.md)
- [Local checks](pr-duty-checks-before-merge-local-checks.md)
- [An approved head whose GitHub checks never arrive is handed to an agent after 2 minutes](pr-duty-checks-before-merge-an-approved-head-whose-github-checks.md)
- [With the "Checks pass" rule off, a review is by reading and Velocity merges on approval, without reading GitHub's checks](pr-duty-checks-before-merge-with-the-checks-pass-rule-off.md)
- [Why](pr-duty-checks-before-merge-why.md)
- [What it gives up](pr-duty-checks-before-merge-what-it-gives-up.md)
