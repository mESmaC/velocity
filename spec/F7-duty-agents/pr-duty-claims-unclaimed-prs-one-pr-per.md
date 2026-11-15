#### PR duty · Claims unclaimed PRs, one PR per agent, so two agents never review the same one

<!-- split-spec: {"order":970,"kind":"markdown","indentation":0} -->
- Claims unclaimed PRs, one PR per agent, so two agents never review the same one.
  The review claim goes through the AIRC lock (`airc_claim` on the PR), so it is atomic.
  Only its holder updates, approves or merges that PR.
  A finding that arrives after the PR merged is filed as an issue.
