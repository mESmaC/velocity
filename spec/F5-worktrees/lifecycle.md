#### Lifecycle (decided)

<!-- split-spec: {"order":617,"kind":"markdown","indentation":0} -->
**Lifecycle (decided):**

| When | What happens |
|---|---|
| A thread starts, or any agent starts a job (PR duty review, scout sweep, swarm member, dispatched issue) | Create a worktree from `master`. Its first transcript step is `Worktree · <path> · created from master`. |
| Its PR merges | Delete the worktree. Delete the local branch too if that setting is on (default on). Log both as steps with the space freed. |
| Its PR is closed without merging | **Keep** the worktree. No automatic expiry. |
| The thread fails | **Keep** the worktree. |
| The user deletes the thread | Delete the thread and its worktree together (F12). |
| Spec interview | No worktree. |

Deletion verifies the saved owning branch and registered checkout and refuses while uncommitted changes remain or commits are not reachable from a remote ref.
A failed verification keeps the checkout and reports why.

A worktree's path is fixed when it is created.
Changing the location setting affects only worktrees created afterwards.
