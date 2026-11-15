#### Warm pool (speed) · Lean

<!-- split-spec: {"order":713,"kind":"markdown","indentation":0} -->
- **Lean.**
  Pool setup installs the project's pinned Lean toolchain (`lean-toolchain`, through elan) and runs `lake exe cache get`.
  Mathlib's built files live in the shared build cache, so a worktree never rebuilds them.

<!-- split-spec links -->

Rule files:

- [Built](warm-pool-lean-built.md)
