#### Warm pool (speed) · Lean

<!-- split-spec: {"order":713,"kind":"markdown","indentation":0} -->
- **Lean.**
  Lean setup runs on demand the first time a workspace builds proofs or runs `lake`, not in every warm-pool slot (#546).
  On demand, elan installs the project's pinned Lean toolchain (`lean-toolchain`) and `lake exe cache get` unpacks Mathlib's prebuilt files from the machine-wide shared cache, so a workspace that never touches `proofs/` carries no `proofs/.lake` and a workspace that builds proofs compiles only the project's own modules.

<!-- split-spec links -->

Rule files:

- [Built](warm-pool-lean-built.md)
