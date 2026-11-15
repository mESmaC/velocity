#### Warm pool (speed) · Swarm launches

<!-- split-spec: {"order":722,"kind":"markdown","indentation":0} -->
- **Swarm launches.**
  Launching a swarm or goal swarm tops the pool up to its agent count.
  Agents that find no ready worktree wait for one rather than racing each other on setup.
  They wait at most 2 minutes, then make their own *(proposed)*.
  Worktrees added beyond the setting are taken as soon as they are checked out, without a prebuild.
