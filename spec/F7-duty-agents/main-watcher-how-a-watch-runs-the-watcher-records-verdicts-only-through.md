#### Main watcher · How a watch runs · The watcher records verdicts only through Velocity's watch MCP tools, served by Velocity's own executable with the watcher's nick and tip fixed by the launcher

<!-- split-spec: {"order":1080,"kind":"markdown","indentation":2} -->
- The watcher records verdicts only through Velocity's watch MCP tools, served by Velocity's own executable with the watcher's nick and tip fixed by the launcher: `watch_green`; `watch_broken` (the breaking merge, its PR, the failing command and output), which checks the commit is on the base branch at the watched tip and, when the project has a GitHub repository, that the PR merged, then posts the `broken` event bound to that commit; and `watch_pull_request`, which checks the fix or revert is an open PR against the base branch and posts a `PR` or `reverted` event.
  A project with no GitHub repository gets the pushed branch named in the watch's summary.
