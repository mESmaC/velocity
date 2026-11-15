#### Rosen · Stored as the agent writes

<!-- split-spec: {"order":551,"kind":"markdown","indentation":0} -->
- **Stored as the agent writes.**
  Velocity runs a write-through watcher on each open workspace, so every write is on the Rosen server within 2 seconds.
  Every 30 seconds it starts a watcher on any open workspace that has none, after a restart or when one ended or failed to start; a watcher's first snapshot covers the whole workspace, so edits made meanwhile are stored then.
  A resumed thread starts its watcher at once.
  There is nothing to commit or push.
