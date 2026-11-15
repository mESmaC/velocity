#### PR duty · How a review runs · A review runs in the PR's review-pr-<n> worktree

<!-- split-spec: {"order":1001,"kind":"markdown","indentation":2} -->
- A review runs in the PR's `review-pr-<n>` worktree: a detached checkout of the PR head with the base branch's tip merged in, so the agent reads, and any local checks run on, the merge result.
  If that merge conflicts, the checkout stays at the PR head and the agent resolves the conflict or sends it back (F6).
