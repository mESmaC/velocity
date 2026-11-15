#### Main watcher · How a watch runs · The runner reads the base branch's tip on the remote at most once a minute

<!-- split-spec: {"order":1078,"kind":"markdown","indentation":2} -->
- The runner reads the base branch's tip on the remote at most once a minute.
  When the tip moves past the last watched commit (and once when the role starts), one idle watcher takes the new tip.
  The others stand by, so a merge that lands mid-watch goes to the next idle watcher and no tip is watched twice.
  Pausing lets a running watch finish and starts none.
  A watch that closing Velocity stopped runs again on the current tip when Velocity starts, unless the role is paused.
