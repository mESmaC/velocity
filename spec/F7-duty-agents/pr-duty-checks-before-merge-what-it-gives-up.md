#### PR duty · Checks before merge · What it gives up

<!-- split-spec: {"order":984,"kind":"markdown","indentation":2} -->
- **What it gives up:** two PRs that each pass can still break `master` together.
  GitHub CI on `master` and the Main watcher catch that after the merge, not before.
