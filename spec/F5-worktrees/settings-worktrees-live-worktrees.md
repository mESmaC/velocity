#### Settings → Worktrees · Live worktrees

<!-- split-spec: {"order":665,"kind":"markdown","indentation":0} -->
- **Live worktrees:** one row per worktree: state dot, path, owner, size and state (working / PR #n / failed · kept / closed · kept / review / sweep / ready / ready · building).
  Warm pool worktrees are listed with the owner "warm pool" and count toward the header and the footer.
  Just-deleted rows show briefly, crossed out.
  The header shows the count, total size and free space on the drive.
  **Prune orphans** runs `git worktree prune`.
  Clicking a row opens its thread.
