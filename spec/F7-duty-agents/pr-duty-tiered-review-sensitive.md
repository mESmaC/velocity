#### PR duty · Tiered review · Sensitive

<!-- split-spec: {"order":1057,"kind":"markdown","indentation":2} -->
- **Sensitive:** touches any path listed under "Sensitive paths" in AGENTS.md (e.g. migrations, worktree deletion, CI config, auth).
  Full review, plus the project's stress suite before merge (the stress command AGENTS.md names as the merge gate; a long benchmark or load run never is one), plus a confidence proof at the reference level for each proof-list feature it changes: a fixture table exported from the Lean model plus one test that checks the code against it.
  The code level is required only when an issue asks for a translation (F17, #142).
