#### PR duty · How a review runs · A Rosen change · Checks in a plain merge tree (#394, #378, #388)

<!-- split-spec: {"order":1033,"kind":"markdown","indentation":4} -->
- **Checks in a plain merge tree (#394, #378, #388).**
  `scripts/check.ps1` accepts `-ChangedPathsFile`, a UTF-8 list of root-relative changed paths, one per line, including deletions.
  Without that list or a local `.git`, it runs every check; `-Full` never needs change detection.
  Bindings are checked against regenerated content without Git, and Git line-ending cleanup runs only in a Git checkout.
  A merge result with `regenerate` paths goes back to its author with the regeneration commands (F5); a checker never certifies the base's generated copies as the change's merged artifacts (#404).
  After a clean land, locked Cargo and pnpm checks and the bindings comparison reject stale generated copies.
  The stress runner locates the Velocity workspace from its current directory or an ancestor at run time, including when a warm-pool binary is reused, and runs every script and workspace check in that tree.
  Git-specific stress evidence creates isolated fixtures from that tree's sources; the merge tree needs no synthetic Git history.
  These PowerShell checks require PowerShell 7 (`pwsh`) on the duty machine's PATH.
