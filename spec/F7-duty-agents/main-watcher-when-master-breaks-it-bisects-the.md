#### Main watcher · When master breaks, it bisects the recent merges, posts a broken event naming the merge, and opens either a fix PR or a revert PR

<!-- split-spec: {"order":1073,"kind":"markdown","indentation":0} -->
- When `master` breaks, it bisects the recent merges, posts a `broken` event naming the merge, and opens either a fix PR or a revert PR.
  That PR goes through PR duty like any other; the watcher never pushes to `master`.
