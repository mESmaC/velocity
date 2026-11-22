#### Rosen · Falling back

<!-- split-spec: {"order":614,"kind":"markdown","indentation":0} -->
- **Falling back.**
  A project never falls back to a git worktree when its Rosen server can't start or its workspace can't be made (#407, #418).
  Ordinary threads and goal swarm members retry transient Rosen errors (pipe busy, connection refused or timeout) with exponential backoff for about a minute.
  A change already allocated or a workspace already made is retained and reused by the retry.
  On a lasting error the thread enters `error`, shows the Rosen error, and starts no agent or git worktree.
  Its right-click menu offers `Retry Rosen workspace`, which retries the same thread and folder with its original prompt and provider, including after restarting Velocity.
